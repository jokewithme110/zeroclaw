use crate::AppState;
use axum::{
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::net::SocketAddr;
use zeroclaw_log::{Action, Event, EventCategory, EventOutcome, record};
use zeroclaw_runtime::dt_nodes_registry::{ConnectedNodeRegistry, handle_node_socket};

pub async fn handle_ws_node(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    _headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !state.config.read().gateway.node_control.enabled {
        record!(
            INFO,
            Event::new("gateway", Action::Reject)
                .with_category(EventCategory::Channel)
                .with_outcome(EventOutcome::Unknown)
                .with_attrs(serde_json::json!({
                    "peer_addr": peer_addr.to_string(),
                })),
            "Node WebSocket connection rejected (node_control.enabled = false)",
        );
        return (
            StatusCode::NOT_FOUND,
            "Node WebSocket is disabled (node_control.enabled = false)",
        )
            .into_response();
    }
    record!(
        INFO,
        Event::new("gateway", Action::Start)
            .with_category(EventCategory::Channel)
            .with_outcome(EventOutcome::Success)
            .with_attrs(serde_json::json!({
                "peer_addr": peer_addr.to_string(),
            })),
        "Node WebSocket connection accepted",
    );
    let registry = ConnectedNodeRegistry::global();
    ws.on_upgrade(move |socket| handle_node_socket(socket, registry, peer_addr, state.pairing))
        .into_response()
}
