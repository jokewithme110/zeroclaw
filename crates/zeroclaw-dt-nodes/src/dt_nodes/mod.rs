use anyhow::Result;
use base64::Engine;
use dialoguer::{Input, Password, Select};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::signal;

mod executor;
mod handlers;
mod node_runtime_trace;
mod ws_client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentityFile {
    pub device_id: String,
    pub public_key_b64: String,
    pub private_key_b64: String,
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone)]
struct DiscoveredGateway {
    host: String,
    port: u16,
    fullname: String,
}

fn default_mdns_service_type() -> String {
    "_zeroclaw-gw._tcp.local.".into()
}

fn discover_gateways_via_mdns(
    service_type: &str,
    timeout: Duration,
) -> Result<Vec<DiscoveredGateway>> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(service_type)?;
    let deadline = Instant::now() + timeout;
    let mut seen = HashSet::<String>::new();
    let mut found = Vec::<DiscoveredGateway>::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait_for = remaining.min(Duration::from_millis(500));
        match receiver.recv_timeout(wait_for) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let port = info.get_port();
                if port == 0 {
                    continue;
                }
                let host = info
                    .get_addresses_v4()
                    .into_iter()
                    .next()
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_string());
                if host.trim().is_empty() {
                    continue;
                }
                let key = format!("{host}:{port}");
                if seen.insert(key) {
                    found.push(DiscoveredGateway {
                        host,
                        port,
                        fullname: info.get_fullname().to_string(),
                    });
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}

fn interactive_gateway_setup(
    config: &zeroclaw_config::schema::Config,
    current_host: Option<String>,
    current_port: Option<u16>,
    current_token: Option<String>,
) -> Result<(String, u16, Option<String>)> {
    let service_type = if config.gateway.mdns.service_type.trim().is_empty() {
        default_mdns_service_type()
    } else {
        config.gateway.mdns.service_type.clone()
    };
    let discovered = discover_gateways_via_mdns(&service_type, Duration::from_secs(5))?;
    let (selected_host, selected_port) = if discovered.is_empty() {
        println!("No gateway discovered via mDNS. Please enter gateway manually.");
        let host: String = Input::new()
            .with_prompt("Gateway host")
            .default(current_host.unwrap_or_else(|| config.gateway.host.clone()))
            .interact_text()?;
        let port: u16 = Input::new()
            .with_prompt("Gateway port")
            .default(current_port.unwrap_or(config.gateway.port))
            .interact_text()?;
        (host, port)
    } else {
        let mut items: Vec<String> = discovered
            .iter()
            .map(|g| format!("{}:{} ({})", g.host, g.port, g.fullname))
            .collect();
        items.push("Manual input".into());
        let selected = Select::new()
            .with_prompt("Select a gateway to connect")
            .items(&items)
            .default(0)
            .interact()?;
        if selected == items.len() - 1 {
            let host: String = Input::new()
                .with_prompt("Gateway host")
                .default(current_host.unwrap_or_else(|| config.gateway.host.clone()))
                .interact_text()?;
            let port: u16 = Input::new()
                .with_prompt("Gateway port")
                .default(current_port.unwrap_or(config.gateway.port))
                .interact_text()?;
            (host, port)
        } else {
            (discovered[selected].host.clone(), discovered[selected].port)
        }
    };
    let token: String = Password::new()
        .with_prompt("Gateway token")
        .allow_empty_password(false)
        .interact()?;
    let final_token = if token.trim().is_empty() {
        current_token
    } else {
        Some(token)
    };
    Ok((selected_host, selected_port, final_token))
}

fn persist_node_config_file(
    config_path: Option<&str>,
    display_name: Option<&str>,
    host: &str,
    port: u16,
    token: Option<&str>,
) -> Result<()> {
    let Some(path) = config_path else {
        return Ok(());
    };
    if path.trim().is_empty() {
        return Ok(());
    }
    let path_buf = PathBuf::from(path);
    if let Some(parent) = path_buf.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let payload = serde_json::json!({
        "display_name": display_name,
        "gateway": { "host": host, "port": port, "token": token }
    });
    std::fs::write(path_buf, serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn identity_path(workspace_dir: &PathBuf) -> PathBuf {
    let mut dir = workspace_dir.clone();
    dir.push("identity");
    std::fs::create_dir_all(&dir).ok();
    dir.push("device.json");
    dir
}

fn load_or_create_identity(
    config: &zeroclaw_config::schema::Config,
    workspace_dir: &PathBuf,
    display_name: &str,
    host: String,
    port: u16,
    token: Option<String>,
) -> Result<NodeIdentityFile> {
    let zeroclaw_dir = config
        .config_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_dir.clone());
    let secret_store =
        zeroclaw_config::secrets::SecretStore::new(&zeroclaw_dir, config.secrets.encrypt);
    let path = identity_path(workspace_dir);
    if path.exists() {
        let data = std::fs::read_to_string(&path)?;
        let mut id: NodeIdentityFile = serde_json::from_str(&data)?;
        id.public_key_b64 = secret_store.decrypt(&id.public_key_b64)?;
        id.private_key_b64 = secret_store.decrypt(&id.private_key_b64)?;
        id.gateway.token = id
            .gateway
            .token
            .as_deref()
            .map(|v| secret_store.decrypt(v))
            .transpose()?;
        id.gateway.host = if id.gateway.host.is_empty() {
            host
        } else {
            id.gateway.host
        };
        id.gateway.port = if id.gateway.port == 0 {
            port
        } else {
            id.gateway.port
        };
        if token.is_some() {
            id.gateway.token = token;
        }
        if !display_name.is_empty() {
            id.display_name = Some(display_name.to_string());
        }
        let mut id_persist = id.clone();
        id_persist.public_key_b64 = secret_store.encrypt(&id.public_key_b64)?;
        id_persist.private_key_b64 = secret_store.encrypt(&id.private_key_b64)?;
        id_persist.gateway.token = id
            .gateway
            .token
            .as_deref()
            .map(|v| secret_store.encrypt(v))
            .transpose()?;
        std::fs::write(&path, serde_json::to_string_pretty(&id_persist)?)?;
        return Ok(id);
    }
    let pub_bytes: [u8; 32] = rand::random();
    let priv_bytes: [u8; 64] = rand::random();
    let id = NodeIdentityFile {
        device_id: format!("zeroclaw-node-{}", uuid::Uuid::new_v4()),
        public_key_b64: base64::engine::general_purpose::STANDARD.encode(pub_bytes),
        private_key_b64: base64::engine::general_purpose::STANDARD.encode(priv_bytes),
        gateway: GatewayConfig { host, port, token },
        display_name: Some(display_name.to_string()),
    };
    let mut id_persist = id.clone();
    id_persist.public_key_b64 = secret_store.encrypt(&id.public_key_b64)?;
    id_persist.private_key_b64 = secret_store.encrypt(&id.private_key_b64)?;
    id_persist.gateway.token = id
        .gateway
        .token
        .as_deref()
        .map(|v| secret_store.encrypt(v))
        .transpose()?;
    std::fs::write(&path, serde_json::to_string_pretty(&id_persist)?)?;
    Ok(id)
}

pub async fn run_node(
    config: &zeroclaw_config::schema::Config,
    interactive: bool,
    init: bool,
    config_path: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    token: Option<String>,
) -> Result<()> {
    #[derive(Deserialize)]
    struct NodeConfigFile {
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        gateway: Option<GatewayConfig>,
    }
    let mut display_name = name.clone();
    let mut cfg_host: Option<String> = None;
    let mut cfg_port: Option<u16> = None;
    let mut cfg_token: Option<String> = None;
    if let Some(path) = config_path.as_deref() {
        if !path.trim().is_empty() {
            let path_buf = PathBuf::from(path);
            if path_buf.exists() {
                let data = std::fs::read_to_string(&path_buf)?;
                let file_cfg: NodeConfigFile = serde_json::from_str(&data)?;
                if let Some(dn) = file_cfg.display_name {
                    if !dn.trim().is_empty() {
                        display_name = Some(dn);
                    }
                }
                if let Some(gw) = file_cfg.gateway {
                    if !gw.host.trim().is_empty() {
                        cfg_host = Some(gw.host);
                    }
                    if gw.port != 0 {
                        cfg_port = Some(gw.port);
                    }
                    cfg_token = gw.token;
                }
            }
        }
    }
    let initial_host = host.or(cfg_host);
    let initial_port = port.or(cfg_port);
    let initial_token = token.or(cfg_token);
    let (gateway_host, gateway_port, final_token) = if interactive {
        interactive_gateway_setup(config, initial_host, initial_port, initial_token)?
    } else {
        (
            initial_host.unwrap_or_else(|| config.gateway.host.clone()),
            initial_port.unwrap_or(config.gateway.port),
            initial_token,
        )
    };
    let workspace_dir = config.workspace_dir.clone();
    let effective_name = display_name.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "zeroclaw-node".to_string())
    });
    if interactive {
        persist_node_config_file(
            config_path.as_deref(),
            Some(effective_name.as_str()),
            &gateway_host,
            gateway_port,
            final_token.as_deref(),
        )?;
    }
    let identity = load_or_create_identity(
        config,
        &workspace_dir,
        &effective_name,
        gateway_host.clone(),
        gateway_port,
        final_token,
    )?;
    if init {
        println!(
            "Initialized node identity at {} (device_id={})",
            identity_path(&workspace_dir).display(),
            identity.device_id
        );
        return Ok(());
    }
    let url = format!("ws://{}:{}/", identity.gateway.host, identity.gateway.port);
    let stop = signal::ctrl_c();
    ws_client::run_loop(url, &identity, stop).await
}
