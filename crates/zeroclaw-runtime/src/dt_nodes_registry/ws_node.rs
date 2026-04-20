use super::node_registry::{ConnectedNodeRegistry, NodeCommandResult, OutgoingMessage};
use axum::extract::ws::{Message, WebSocket};
use axum::http::HeaderMap;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::time::{self, Duration, MissedTickBehavior};
use uuid::Uuid;
use crate::security::pairing::PairingGuard;

const NODE_TICK_INTERVAL_MS: u64 = 30_000;

pub fn sanitize_ws_headers(headers: &HeaderMap) -> Value {
    let mut out = serde_json::Map::new();
    for (key, value) in headers {
        let k = key.as_str().to_ascii_lowercase();
        let v = value.to_str().unwrap_or("<non-utf8>");
        let redacted = matches!(
            k.as_str(),
            "authorization" | "x-node-control-token" | "cookie" | "set-cookie"
        );
        out.insert(
            key.as_str().to_string(),
            Value::String(if redacted {
                "<redacted>".to_string()
            } else {
                v.to_string()
            }),
        );
    }
    Value::Object(out)
}

pub async fn handle_node_socket(
    mut socket: WebSocket,
    registry: std::sync::Arc<ConnectedNodeRegistry>,
    peer_addr: SocketAddr,
    pairing: std::sync::Arc<PairingGuard>,
) {
    let nonce = Uuid::new_v4().to_string();
    let challenge = serde_json::json!({
        "type": "event",
        "event": "connect.challenge",
        "payload": { "nonce": nonce },
    });
    if socket
        .send(Message::Text(challenge.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let (node_id, mut out_rx) = loop {
        let msg = match socket.recv().await {
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            _ => continue,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let frame_type = parsed["type"].as_str().unwrap_or("");
        let method = parsed["method"].as_str().unwrap_or("");
        if frame_type != "req" || method != "connect" {
            continue;
        }
        let connect_id = match parsed["id"].as_str().map(str::trim) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        let params = parsed.get("params").cloned().unwrap_or(serde_json::json!({}));
        if pairing.require_pairing() {
            let provided = params
                .get("auth")
                .and_then(|v| v.get("token"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            if !pairing.is_authenticated(provided) {
                let connect_res = serde_json::json!({
                    "type": "res",
                    "id": connect_id,
                    "ok": false,
                    "error": { "code": "unauthorized", "message": "invalid auth.token" }
                });
                let _ = socket
                    .send(Message::Text(connect_res.to_string().into()))
                    .await;
                return;
            }
        }
        let role = params
            .get("role")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("node");
        if role != "node" {
            let connect_res = serde_json::json!({
                "type":"res","id":connect_id,"ok":false,
                "error":{"code":"invalid_request","message":format!("unsupported role: {role} (only 'node' is allowed)")}
            });
            let _ = socket
                .send(Message::Text(connect_res.to_string().into()))
                .await;
            return;
        }
        let device_id = params
            .get("device")
            .and_then(|d| d.get("id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let client_id = params
            .get("client")
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let node_id = match device_id.or(client_id) {
            Some(id) => id,
            None => continue,
        };
        let mut capabilities: Vec<String> = Vec::new();
        if let Some(caps) = params.get("caps").and_then(|v| v.as_array()) {
            for v in caps {
                if let Some(s) = v.as_str().map(str::trim) {
                    if !s.is_empty() {
                        capabilities.push(s.to_string());
                    }
                }
            }
        }
        if let Some(commands) = params.get("commands").and_then(|v| v.as_array()) {
            for v in commands {
                if let Some(s) = v.as_str().map(str::trim) {
                    if !s.is_empty() {
                        capabilities.push(s.to_string());
                    }
                }
            }
        }
        let mut meta = params.clone();
        if let Some(obj) = meta.as_object_mut() {
            obj.insert(
                "remoteIp".to_string(),
                serde_json::Value::String(peer_addr.ip().to_string()),
            );
        }
        let rx = registry.register(node_id.clone(), capabilities, Some(meta));
        let connect_res = serde_json::json!({
            "type":"res","id":connect_id,"ok":true,"payload":{"nodeId":node_id}
        });
        if socket
            .send(Message::Text(connect_res.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        break (node_id, rx);
    };

    let mut tick_interval = time::interval(Duration::from_millis(NODE_TICK_INTERVAL_MS));
    tick_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            msg = socket.recv() => {
                let msg = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => continue,
                };
                let parsed: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let frame_type = parsed["type"].as_str().unwrap_or("");
                let method = parsed["method"].as_str().unwrap_or("");
                if frame_type == "req" && method == "node.invoke.result" {
                    let params = parsed.get("params").cloned().unwrap_or(serde_json::json!({}));
                    let request_id = params["id"].as_str().unwrap_or("").to_string();
                    if request_id.is_empty() { continue; }
                    let ok = params["ok"].as_bool().unwrap_or(false);
                    let payload_json = params["payloadJSON"].as_str().map(String::from);
                    let payload_value = params.get("payload").cloned();
                    let error_value = params.get("error").cloned();
                    let output = if let Some(pj) = payload_json { pj } else if let Some(pv) = payload_value { pv.to_string() } else { String::new() };
                    let error = error_value.map(|e| e.to_string());
                    let result = NodeCommandResult { success: ok, output, error };
                    registry.complete_pending(&request_id, result);
                }
            }
            out_msg = out_rx.recv() => {
                let out = match out_msg {
                    Some(m) => m,
                    None => break,
                };
                let wire = match &out {
                    OutgoingMessage::Invoke { request_id, capability, arguments } => {
                        let params_json = arguments.to_string();
                        let payload = serde_json::json!({
                            "id": request_id,
                            "nodeId": node_id,
                            "command": capability,
                            "paramsJSON": params_json,
                            "timeoutMs": 15_000i64,
                        });
                        serde_json::json!({"type":"event","event":"node.invoke.request","payload":payload})
                    }
                    OutgoingMessage::Run { request_id, command } => {
                        let params = serde_json::json!({"command": Vec::<String>::new(),"rawCommand": command});
                        let params_json = params.to_string();
                        let payload = serde_json::json!({
                            "id": request_id,
                            "nodeId": node_id,
                            "command": "system.run",
                            "paramsJSON": params_json,
                            "timeoutMs": 15_000i64,
                        });
                        serde_json::json!({"type":"event","event":"node.invoke.request","payload":payload})
                    }
                };
                if socket.send(Message::Text(wire.to_string().into())).await.is_err() {
                    break;
                }
            }
            _ = tick_interval.tick() => {
                let payload = serde_json::json!({"ts": chrono::Utc::now().timestamp_millis()});
                let tick = serde_json::json!({"type":"event","event":"tick","payload":payload});
                if socket.send(Message::Text(tick.to_string().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    registry.unregister(&node_id);
}
