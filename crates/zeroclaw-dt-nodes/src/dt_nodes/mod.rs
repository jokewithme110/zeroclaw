//! zeroclaw-dt-nodes 运行时逻辑

#[cfg(feature = "auto_discovery")]
use crate::config::GatewayEndpoint;
#[cfg(feature = "auto_discovery")]
use crate::config::NodeIdentity;
use crate::config::{NodeConfig as LocalNodeConfig, resolve_local_node_identity_context};
#[cfg(feature = "auto_discovery")]
use crate::config::{resolve_local_auto_discovery_context, resolve_local_node_profile_context};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(feature = "auto_discovery")]
use std::path::PathBuf;
use tokio::signal;

mod executor;
#[cfg(feature = "auto_discovery")]
mod fifo_listener;
pub mod handlers;
mod node_client;
mod node_runtime_trace;

pub use handlers::event_store::EventSubscriptionsStore;

/// Event management subcommands
#[derive(clap::Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventCommands {
    /// List event subscriptions
    #[command(long_about = "\
List event subscriptions.

Shows the topic, channel, recipient and subscription timestamp for each \
recorded event subscription.

Examples:
  zeroclaw-dt-nodes event list                  # list all subscriptions
  zeroclaw-dt-nodes event list --event alert.cpu # filter by event type
  zeroclaw-dt-nodes event list --json           # output as JSON")]
    List {
        /// Filter by event type (topic)
        #[arg(long)]
        event: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct LocalStartOptions {
    pub interactive: bool,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub token: Option<String>,
    #[cfg(feature = "auto_discovery")]
    pub auto_discovery: Option<bool>,
    #[cfg(feature = "auto_discovery")]
    pub fifo_dir: Option<String>,
    #[cfg(feature = "auto_discovery")]
    pub fifo_wait_timeout_secs: Option<u64>,
}

/// 独立 zeroclaw-dt-nodes 二进制入口。
pub async fn run_node(config: &mut LocalNodeConfig, options: &LocalStartOptions) -> Result<()> {
    #[cfg(feature = "auto_discovery")]
    let auto_discovery_context = resolve_local_auto_discovery_context(
        config,
        options.auto_discovery,
        options.fifo_dir.clone(),
        options.fifo_wait_timeout_secs,
    );

    #[cfg(feature = "auto_discovery")]
    if auto_discovery_context.enabled {
        let profile_context = resolve_local_node_profile_context(options.name.clone());
        let identity = config.load_or_create_identity_profile(&profile_context)?;
        let discovery_config = config.clone();
        let fifo_dir = auto_discovery_context.fifo_dir;
        let fifo_wait_timeout_secs = auto_discovery_context.fifo_wait_timeout_secs;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_category(::zeroclaw_log::EventCategory::System)
                .with_attrs(::serde_json::json!({
                    "event": "node_auto_discovery_enabled",
                    "fifo_dir": fifo_dir,
                    "fifo_wait_timeout_secs": fifo_wait_timeout_secs,
                })),
            "Starting node with FIFO auto-discovery"
        );
        return run_with_fifo_listener(
            &fifo_dir,
            fifo_wait_timeout_secs,
            &identity,
            Some(config.zeroclaw_node_dir.clone()),
            Some(discovery_config),
        )
        .await;
    }

    let identity_context = resolve_local_node_identity_context(
        config,
        options.interactive,
        options.host.clone(),
        options.port,
        options.name.clone(),
        options.token.clone(),
    )?;

    if options.interactive {
        config.update_gateway_endpoint(identity_context.gateway.clone())?;
        println!("配置已更新：{}", config.config_path.display());
    }

    let identity = config.load_or_create_identity_with_context(&identity_context)?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "event": "node_starting_static",
                "host": identity.host.as_str(),
                "port": identity.port,
            })
        ),
        "Connecting to gateway with static configuration"
    );
    let url = format!("ws://{}:{}/", identity.host, identity.port);
    let stop = signal::ctrl_c();
    node_client::run_loop(url, &identity, Some(config.zeroclaw_node_dir.clone()), stop).await
}

/// 处理 event 命令
pub fn handle_event_command(command: EventCommands, workspace_dir: &Path) -> anyhow::Result<()> {
    match command {
        EventCommands::List { event, json } => {
            let store = EventSubscriptionsStore::new(workspace_dir)
                .context("Failed to open event subscriptions store")?;

            let subscriptions = store
                .list_subscriptions(event.as_deref(), None, None)
                .context("Failed to list subscriptions")?;

            if subscriptions.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("No event subscriptions found.");
                }
                return Ok(());
            }

            if json {
                let json_output = serde_json::to_string_pretty(&subscriptions)
                    .context("Failed to serialize subscriptions to JSON")?;
                println!("{}", json_output);
            } else {
                println!(
                    "{:<8} {:<20} {:<10} {:<35} Subscribed At",
                    "ID", "Topic", "Channel", "Recipient"
                );
                println!("{:-<8} {:-<20} {:-<10} {:-<35} {:-<20}", "", "", "", "", "");

                for sub in subscriptions {
                    println!(
                        "{:<8} {:<20} {:<10} {:<35} {}",
                        sub.id,
                        sub.topic,
                        sub.channel,
                        sub.recipient,
                        sub.subscribed_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }

            Ok(())
        }
    }
}

/// Run node with FIFO-based auto-discovery.
#[cfg(feature = "auto_discovery")]
async fn run_with_fifo_listener(
    fifo_dir: &str,
    timeout_secs: u64,
    base_identity: &NodeIdentity,
    workspace_dir: Option<PathBuf>,
    discovery_config: Option<LocalNodeConfig>,
) -> Result<()> {
    use fifo_listener::{GatewayInfo, run_fifo_listener};

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel_token.clone();
    let base_identity = base_identity.clone();

    let fifo_fut = run_fifo_listener(
        fifo_dir,
        timeout_secs,
        cancel_token.clone(),
        move |gateway_info: GatewayInfo| {
            let cancel_token_inner = cancel_token.child_token();
            let workspace_dir = workspace_dir.clone();
            let discovery_config = discovery_config.clone();
            let base_identity = base_identity.clone();

            async move {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "event": "node_connecting_to_gateway",
                            "ip": gateway_info.ip.as_str(),
                            "port": gateway_info.port,
                        })),
                    "Connecting to discovered gateway"
                );

                let gateway_endpoint = GatewayEndpoint::new(
                    gateway_info.ip.clone(),
                    gateway_info.port,
                    Some(gateway_info.token.clone()),
                );
                if let Some(mut discovery_config) = discovery_config {
                    discovery_config.persist_discovered_gateway_endpoint(&gateway_endpoint)?;
                }
                let discovered_identity =
                    base_identity.with_gateway_endpoint(gateway_endpoint.clone());
                let ws_url = gateway_endpoint.websocket_url();
                let cancel_for_ws = cancel_token_inner.clone();

                let ws_fut = node_client::run_loop(
                    ws_url,
                    &discovered_identity,
                    workspace_dir,
                    async move {
                        cancel_for_ws.cancelled().await;
                        Ok(())
                    },
                );

                tokio::select! {
                    result = ws_fut => result,
                    _ = cancel_token_inner.cancelled() => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(::serde_json::json!({
                                    "event": "node_websocket_cancelled",
                                })),
                            "WebSocket cancelled by shutdown signal"
                        );
                        Ok(())
                    }
                }
            }
        },
    );
    tokio::pin!(fifo_fut);

    tokio::select! {
        _ = signal::ctrl_c() => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "event": "node_shutdown_signal",
                    })),
                "Received shutdown signal"
            );
            cancel_clone.cancel();
            if let Err(e) = (&mut fifo_fut).await {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "event": "node_fifo_listener_error",
                            "error": e.to_string(),
                        })),
                    "FIFO listener failed during shutdown"
                );
            }
        }
        result = &mut fifo_fut => {
            if let Err(e) = result {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "event": "node_fifo_listener_error",
                            "error": e.to_string(),
                        })),
                    "FIFO listener failed"
                );
            }
        }
    }

    Ok(())
}
