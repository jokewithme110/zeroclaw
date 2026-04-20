use anyhow::Result;
use base64::Engine;
use dialoguer::{Input, Password};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

fn identity_path(workspace_dir: &Path) -> PathBuf {
    let mut dir = workspace_dir.to_path_buf();
    dir.push("identity");
    std::fs::create_dir_all(&dir).ok();
    dir.push("device.json");
    dir
}

fn load_or_create_identity(
    config: &zeroclaw_config::schema::Config,
    workspace_dir: &Path,
    display_name: &str,
    host: String,
    port: u16,
    token: Option<String>,
    update_gateway: bool,
) -> Result<NodeIdentityFile> {
    let zeroclaw_dir = config
        .config_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_dir.to_path_buf());
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
        // 仅在交互式模式下更新 gateway 配置为用户新输入的值
        if update_gateway {
            id.gateway.host = host;
            id.gateway.port = port;
            id.gateway.token = token;
        }
        // Re-encrypt keys (they were decrypted above for use but need to be stored encrypted)
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
    let gateway_host: String;
    let gateway_port: u16;
    let final_token: Option<String>;
    if interactive {
        gateway_host = Input::new()
            .with_prompt("Gateway host")
            .default(initial_host.unwrap_or_else(|| config.gateway.host.clone()))
            .interact_text()?;
        gateway_port = Input::new()
            .with_prompt("Gateway port")
            .default(initial_port.unwrap_or(config.gateway.port))
            .interact_text()?;
        let tok: String = Password::new()
            .with_prompt("Gateway token")
            .allow_empty_password(false)
            .interact()?;
        final_token = Some(tok);
    } else {
        // 非交互式模式：优先从 device.json 读取 gateway 配置
        let workspace_dir = config.data_dir.clone();
        let id_path = identity_path(&workspace_dir);
        if id_path.exists() {
            if let Ok(data) = std::fs::read_to_string(&id_path) {
                if let Ok(id_file) = serde_json::from_str::<NodeIdentityFile>(&data) {
                    gateway_host = id_file.gateway.host;
                    gateway_port = id_file.gateway.port;
                    final_token = id_file.gateway.token;
                } else {
                    gateway_host = initial_host.unwrap_or_else(|| config.gateway.host.clone());
                    gateway_port = initial_port.unwrap_or(config.gateway.port);
                    final_token = initial_token;
                }
            } else {
                gateway_host = initial_host.unwrap_or_else(|| config.gateway.host.clone());
                gateway_port = initial_port.unwrap_or(config.gateway.port);
                final_token = initial_token;
            }
        } else {
            gateway_host = initial_host.unwrap_or_else(|| config.gateway.host.clone());
            gateway_port = initial_port.unwrap_or(config.gateway.port);
            final_token = initial_token;
        }
    };
    let workspace_dir = config.data_dir.clone();
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
        interactive,
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
