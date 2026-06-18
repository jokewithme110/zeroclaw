//! Chat command - send message to webchat channel (non-streaming)

use crate::dt_nodes::handlers::event_store::EventSubscriptionsStore;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct ChatRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_req: Option<NodeRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NodeRequest {
    event: String,
    channel_id: String,
    recipient: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<ImageData>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ImageData {
    filename: String,
    base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// 将图片文件转换为 base64 编码
fn image_to_base64(path: &str) -> Result<(String, String)> {
    let path_buf = PathBuf::from(path);
    let filename = path_buf
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();

    let extension = path_buf
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();

    let mime_type = match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };

    let image_data =
        fs::read(path).with_context(|| format!("Failed to read image file: {}", path))?;
    let base64_data = STANDARD.encode(&image_data);

    Ok((
        format!("data:{};base64,{}", mime_type, base64_data),
        filename,
    ))
}

/// 处理图片路径列表，转换为 ImageData 列表
fn process_images(images: &[String]) -> Result<Option<Vec<ImageData>>> {
    if images.is_empty() {
        return Ok(None);
    }

    let image_data_list: Result<Vec<ImageData>> = images
        .iter()
        .map(|path| {
            let (base64, filename) = image_to_base64(path)?;
            Ok(ImageData { filename, base64 })
        })
        .collect();

    image_data_list.map(Some)
}

/// 构建聊天请求
fn build_chat_request(
    message: &str,
    session_id: String,
    node_req: Option<NodeRequest>,
) -> ChatRequest {
    ChatRequest {
        model: None,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        }],
        stream: false,
        session_id: Some(session_id),
        node_req,
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Choice {
    index: i32,
    message: ResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseMessage {
    role: String,
    content: String,
}

/// Send a message to webchat channel and wait for non-streaming response
pub async fn send_to_webchat(
    gateway_url: &str,
    message: &str,
    images: &[String],
    token: Option<&str>,
) -> Result<()> {
    let client = Client::new();

    // 处理图片
    let image_data_list = process_images(images)?;

    let node_req = image_data_list.map(|imgs| NodeRequest {
        event: String::new(),
        channel_id: String::new(),
        recipient: String::new(),
        message: message.to_string(),
        images: Some(imgs),
    });

    let request = build_chat_request(message, Uuid::new_v4().to_string(), node_req);

    let mut req = client.post(gateway_url).json(&request);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    let response = req
        .send()
        .await
        .context("Failed to send request to webchat")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!(
            "Webchat request failed with status {}: {}",
            status,
            error_text
        );
    }

    let chat_response: ChatResponse = response
        .json()
        .await
        .context("Failed to parse webchat response")?;

    if let Some(choice) = chat_response.choices.first() {
        println!("{}", choice.message.content);
    }

    Ok(())
}

/// Emit an event to all subscribed channels and recipients
pub async fn emit_event(
    gateway_url: &str,
    zeroclaw_node_dir: &Path,
    event: &str,
    message: &str,
    images: &[String],
    token: Option<&str>,
) -> Result<()> {
    // Use existing EventSubscriptionsStore to query subscriptions
    let store = EventSubscriptionsStore::new(zeroclaw_node_dir)?;
    let subscriptions = store.list_subscriptions(Some(event), None, None)?;

    if subscriptions.is_empty() {
        println!("No subscriptions found for event '{}'", event);
        return Ok(());
    }

    println!(
        "Sending event '{}' to {} subscription(s)...",
        event,
        subscriptions.len()
    );

    // 处理图片
    let image_data_list = process_images(images)?;

    let client = Client::new();
    let mut success_count = 0;
    let mut fail_count = 0;

    for sub in &subscriptions {
        let node_req = NodeRequest {
            event: event.to_string(),
            channel_id: sub.channel.clone(),
            recipient: sub.recipient.clone(),
            message: message.to_string(),
            images: image_data_list.clone(),
        };

        let request = build_chat_request(
            message,
            format!("{}:{}:{}", sub.channel, sub.recipient, Uuid::new_v4()),
            Some(node_req),
        );

        let mut req = client.post(gateway_url).json(&request);
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {}", t));
        }

        match req.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    success_count += 1;
                    println!("  [OK] {}:{} - event sent", sub.channel, sub.recipient);
                } else {
                    fail_count += 1;
                    let error_text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    println!(
                        "  [FAIL] {}:{} - status: {}, error: {}",
                        sub.channel, sub.recipient, status, error_text
                    );
                }
            }
            Err(e) => {
                fail_count += 1;
                println!("  [FAIL] {}:{} - {}", sub.channel, sub.recipient, e);
            }
        }
    }

    println!(
        "Event '{}' completed: {} sent, {} failed",
        event, success_count, fail_count
    );
    Ok(())
}
