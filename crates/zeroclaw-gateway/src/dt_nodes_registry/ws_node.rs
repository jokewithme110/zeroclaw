use crate::AppState;
use axum::{
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::net::SocketAddr;
use zeroclaw_runtime::dt_nodes_registry::{
    ConnectedNodeRegistry, handle_node_socket, sanitize_ws_headers,
};

pub async fn handle_ws_node(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    tracing::info!(
        peer = %peer_addr,
        headers = %sanitize_ws_headers(&headers),
        "node websocket upgrade request received"
    );
    if !state.config.lock().gateway.node_control.enabled {
        return (
            StatusCode::NOT_FOUND,
            "Node WebSocket is disabled (node_control.enabled = false)",
        )
            .into_response();
    }
    let registry = ConnectedNodeRegistry::global();
    ws.on_upgrade(move |socket| handle_node_socket(socket, registry, peer_addr, state.pairing))
        .into_response()
}
