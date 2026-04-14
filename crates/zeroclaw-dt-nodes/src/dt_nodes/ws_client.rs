use crate::dt_nodes::{
    NodeIdentityFile, executor,
    node_runtime_trace::{self, NodeTraceCtx},
};
use anyhow::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::select;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use zeroclaw_log::{Action, Event, EventCategory, EventOutcome};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

pub async fn run_loop(
    url: String,
    identity: &NodeIdentityFile,
    stop: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<()> {
    tokio::pin!(stop);
    let retry_interval = Duration::from_secs(5);
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        zeroclaw_log::record!(
            INFO,
            Event::new(module_path!(), Action::Connect)
                .with_category(EventCategory::System)
                .with_attrs(json!({ "url": url, "attempt": attempt })),
            "zeroclaw node connecting"
        );
        let mut request = match url.clone().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                zeroclaw_log::record!(
                    ERROR,
                    Event::new(module_path!(), Action::Fail)
                        .with_category(EventCategory::System)
                        .with_outcome(EventOutcome::Failure)
                        .with_attrs(json!({ "url": url, "err": e.to_string() })),
                    "zeroclaw node invalid WebSocket URL"
                );
                return Err(e.into());
            }
        };
        if let Some(token) = &identity.gateway.token {
            if let Ok(value) = HeaderValue::from_str(token) {
                request.headers_mut().insert("X-Node-Control-Token", value);
            }
        }
        let connect_fut = connect_async_tls_with_config(request, None, false, None);
        let ws_stream: WsStream = select! {
                res = connect_fut => {
                match res {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        zeroclaw_log::record!(
                            WARN,
                            Event::new(module_path!(), Action::Fail)
                                .with_category(EventCategory::System)
                                .with_outcome(EventOutcome::Failure)
                                .with_attrs(json!({ "err": e.to_string() })),
                            "zeroclaw node failed to connect"
                        );
                        select! {
                            _ = &mut stop => {
                                zeroclaw_log::record!(
                                    INFO,
                                    Event::new(module_path!(), Action::Cancel)
                                        .with_category(EventCategory::System),
                                    "zeroclaw node received shutdown signal during connect"
                                );
                                return Ok(());
                            }
                            _ = sleep(retry_interval) => { continue; }
                        }
                    }
                }
            }
            _ = &mut stop => {
                zeroclaw_log::record!(
                    INFO,
                    Event::new(module_path!(), Action::Cancel)
                        .with_category(EventCategory::System),
                    "zeroclaw node received shutdown signal before connect"
                );
                return Ok(());
            }
        };
        let (mut sink, mut stream) = ws_stream.split();
        let trace_ctx = NodeTraceCtx {
            req_id: "node_ws_session",
            node_id: &identity.device_id,
        };
        node_runtime_trace::ws_connected(
            &trace_ctx,
            &identity.gateway.host,
            identity.gateway.port,
            attempt,
        );
        let session_result: Result<()> = loop {
            select! {
                biased;
                _ = &mut stop => {
                    zeroclaw_log::record!(
                        INFO,
                        Event::new(module_path!(), Action::Cancel)
                            .with_category(EventCategory::System),
                        "zeroclaw node received shutdown signal"
                    );
                    let _ = sink.send(Message::Close(None)).await;
                    break Ok(());
                }
                msg = stream.next() => {
                    let Some(message) = msg else {
                        zeroclaw_log::record!(
                            INFO,
                            Event::new(module_path!(), Action::Disconnect)
                                .with_category(EventCategory::System),
                            "zeroclaw node websocket stream ended"
                        );
                        break Ok(());
                    };
                    let message = match message {
                        Ok(message) => message,
                        Err(e) => break Err(e.into()),
                    };
                    let text = match message {
                        Message::Text(text) => text,
                        Message::Close(frame) => {
                            zeroclaw_log::record!(
                                INFO,
                                Event::new(module_path!(), Action::Disconnect)
                                    .with_category(EventCategory::System)
                                    .with_attrs(json!({ "frame": format!("{:?}", frame) })),
                                "zeroclaw node websocket closed by server"
                            );
                            break Ok(());
                        }
                        _ => {
                            zeroclaw_log::record!(
                                WARN,
                                Event::new(module_path!(), Action::Note)
                                    .with_category(EventCategory::System)
                                    .with_attrs(json!({ "message": format!("{:?}", message) })),
                                "zeroclaw node websocket received unexpected message"
                            );
                            continue;
                        }
                    };
                    let parsed: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let frame_type = parsed["type"].as_str().unwrap_or("");
                    let event = parsed["event"].as_str().unwrap_or("");
                    if frame_type == "event" && event == "connect.challenge" {
                        let nonce = parsed["payload"]["nonce"].as_str().unwrap_or("").to_string();
                        let connect = build_connect_request(identity, nonce);
                        if let Err(e) = sink.send(Message::Text(connect.to_string().into())).await {
                            zeroclaw_log::record!(
                                ERROR,
                                Event::new(module_path!(), Action::Fail)
                                    .with_category(EventCategory::System)
                                    .with_outcome(EventOutcome::Failure)
                                    .with_attrs(json!({ "err": e.to_string() })),
                                "zeroclaw node failed to send connect frame"
                            );
                            break Err(Error::msg(format!("failed to send connect frame: {e}")));
                        }
                    } else if frame_type == "event" && event == "node.invoke.request" {
                        if let Err(e) = handle_invoke_request(&mut sink, identity, &parsed).await {
                            break Err(e);
                        }
                    }
                }
            }
        };
        if let Err(e) = session_result {
            zeroclaw_log::record!(
                WARN,
                Event::new(module_path!(), Action::Fail)
                    .with_category(EventCategory::System)
                    .with_outcome(EventOutcome::Failure)
                    .with_attrs(json!({ "err": e.to_string() })),
                "zeroclaw node session ended with error"
            );
            node_runtime_trace::ws_session_ended(
                &identity.device_id,
                "error",
                Some(&e.to_string()),
            );
        } else {
            zeroclaw_log::record!(
                INFO,
                Event::new(module_path!(), Action::Complete)
                    .with_category(EventCategory::System)
                    .with_outcome(EventOutcome::Success),
                "zeroclaw node session ended; will retry unless stopped"
            );
            node_runtime_trace::ws_session_ended(&identity.device_id, "closed", None);
        }
        select! {
            _ = &mut stop => {
                zeroclaw_log::record!(
                    INFO,
                    Event::new(module_path!(), Action::Cancel)
                        .with_category(EventCategory::System),
                    "zeroclaw node stopping after session exit"
                );
                return Ok(());
            }
            _ = sleep(retry_interval) => { continue; }
        }
    }
}

fn build_connect_request(identity: &NodeIdentityFile, nonce: String) -> serde_json::Value {
    let client = serde_json::json!({
        "id": "zeroclaw-node",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "mode": "node",
        "displayName": identity.display_name.as_deref().unwrap_or("zeroclaw-node"),
    });
    let device = serde_json::json!({ "id": identity.device_id, "nonce": nonce });
    let caps = vec!["system", "file", "camera"];
    let commands = vec!["system.run", "media.saveImage", "camera.snap"];
    let auth = serde_json::json!({ "token": identity.gateway.token.clone() });
    serde_json::json!({
        "type": "req",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": "connect",
        "params": {
            "role": "node",
            "client": client,
            "device": device,
            "caps": caps,
            "commands": commands,
            "auth": auth,
        }
    })
}

async fn handle_invoke_request(
    sink: &mut WsSink,
    identity: &NodeIdentityFile,
    parsed: &Value,
) -> Result<()> {
    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);
    let req_id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let params_json = payload
        .get("paramsJSON")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "{}".to_string());
    if req_id.is_empty() || command.is_empty() {
        zeroclaw_log::record!(
            WARN,
            Event::new(module_path!(), Action::Note)
                .with_category(EventCategory::Channel)
                .with_attrs(json!({ "payload": payload })),
            "node.invoke.request missing id or command"
        );
        return Ok(());
    }
    zeroclaw_log::record!(
        INFO,
        Event::new(module_path!(), Action::Receive)
            .with_category(EventCategory::Channel)
            .with_attrs(json!({
                "req_id": req_id,
                "command": command,
                "node_id": identity.device_id,
            })),
        "dt_nodes: node.invoke.request accepted"
    );
    let trace_ctx = NodeTraceCtx {
        req_id: req_id.as_str(),
        node_id: identity.device_id.as_str(),
    };
    node_runtime_trace::invoke_started(&trace_ctx, &command);
    let outcome = executor::handle_invoke(&command, &params_json, Some(&trace_ctx)).await;
    node_runtime_trace::invoke_completed(&trace_ctx, &command, &outcome);
    zeroclaw_log::record!(
        INFO,
        Event::new(module_path!(), Action::Complete)
            .with_category(EventCategory::Channel)
            .with_outcome(if outcome.ok {
                EventOutcome::Success
            } else {
                EventOutcome::Failure
            })
            .with_attrs(json!({
                "req_id": req_id,
                "command": command,
                "ok": outcome.ok,
            })),
        "dt_nodes: node.invoke completed, sending result"
    );
    let mut params_map = serde_json::Map::new();
    params_map.insert("id".to_string(), Value::String(req_id.clone()));
    params_map.insert(
        "nodeId".to_string(),
        Value::String(identity.device_id.clone()),
    );
    params_map.insert("ok".to_string(), Value::Bool(outcome.ok));
    if let Some(pj) = outcome.payload_json {
        params_map.insert("payloadJSON".to_string(), Value::String(pj));
    }
    if let Some(err) = outcome.error {
        params_map.insert("error".to_string(), err);
    }
    let res = serde_json::json!({
        "type": "req",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": "node.invoke.result",
        "params": Value::Object(params_map),
    });
    sink.send(Message::Text(res.to_string().into()))
        .await
        .map_err(|e| {
            zeroclaw_log::record!(
                ERROR,
                Event::new(module_path!(), Action::Fail)
                    .with_category(EventCategory::Channel)
                    .with_outcome(EventOutcome::Failure)
                    .with_attrs(json!({ "err": e.to_string() })),
                "zeroclaw node failed to send node.invoke.result"
            );
            Error::msg(format!("failed to send node.invoke.result: {e}"))
        })?;
    Ok(())
}
