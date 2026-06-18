//! 节点配置结构和管理
//!
//! 配置目录结构：
//! ```text
//! $ZEROCLAW_NODE_CONFIG_DIR/.zeroclaw_node/
//! ├── config.toml          # 配置文件
//! └── identity/
//!     └── device.json      # 节点身份（明文存储）
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zeroclaw_config::{schema::SecretsConfig, secrets::SecretStore};

/// 默认配置目录环境变量
const ZEROCLAW_NODE_CONFIG_DIR_ENV: &str = "ZEROCLAW_NODE_CONFIG_DIR";

/// 配置子目录名
const CONFIG_SUBDIR: &str = ".zeroclaw_node";

/// 配置文件名
const CONFIG_FILE: &str = "config.toml";

/// 网关配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub node_control: NodeControlConfig,
}

/// 事件目的地配置（用于 chat/event-emit 命令）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDestinationConfig {
    /// 事件目的地端口
    pub port: u16,
    /// 事件目的地路径
    pub path: String,
}

/// 节点启动自动发现配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDiscoveryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_fifo_dir")]
    pub fifo_dir: String,
    #[serde(default = "default_fifo_wait_timeout_secs")]
    pub fifo_wait_timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_announce_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_intf: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeControlConfig {
    #[serde(default)]
    pub auto_discovery: AutoDiscoveryConfig,
}

/// 节点身份文件结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentityFile {
    pub device_id: String,
    pub public_key_b64: String,
    pub private_key_b64: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// 运行时使用的节点身份（已解密）
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub device_id: String,
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub display_name: Option<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEndpoint {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

impl NodeIdentity {
    pub fn with_gateway_endpoint(&self, gateway: GatewayEndpoint) -> Self {
        let mut identity = self.clone();
        identity.host = gateway.host;
        identity.port = gateway.port;
        identity.token = gateway.token;
        identity
    }
}

impl GatewayEndpoint {
    pub fn new(host: String, port: u16, token: Option<String>) -> Self {
        Self { host, port, token }
    }

    pub fn websocket_url(&self) -> String {
        format!("ws://{}:{}/", self.host, self.port)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedNodeIdentityContext {
    pub display_name: String,
    pub display_name_is_explicit: bool,
    pub gateway: GatewayEndpoint,
}

#[derive(Debug, Clone)]
pub struct ResolvedNodeProfileContext {
    pub display_name: String,
    pub display_name_is_explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAutoDiscoveryContext {
    pub enabled: bool,
    pub fifo_dir: String,
    pub fifo_wait_timeout_secs: u64,
}

fn default_display_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "zeroclaw-node".to_string())
}

fn default_fifo_dir() -> String {
    "/var".to_string()
}

fn default_fifo_wait_timeout_secs() -> u64 {
    300
}

impl Default for AutoDiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fifo_dir: default_fifo_dir(),
            fifo_wait_timeout_secs: default_fifo_wait_timeout_secs(),
            gateway_announce_retries: None,
            ip_intf: None,
        }
    }
}

const DEFAULT_NODE_CONFIG: &str = r#"# ZeroClaw 节点配置文件
# 生成后请根据实际需求修改

# 网关配置
[gateway]
host = "127.0.0.1"
port = 42617
# token = "your-gateway-token-here"
# events = ["router.alert"]  # 可自定义订阅的事件列表

# 事件目的地配置（用于 chat/event-emit 命令）
# host 与 gateway 保持一致，只需配置 port 和 path
[event_destination]
port = 42618
path = "/response"

# 仅自动发现回写到 [gateway].token 的 token 会受此开关影响。
# 静态/手工配置的 gateway.token 始终按明文处理。
[secrets]
encrypt = true

# 节点启动自动发现配置
[gateway.node_control.auto_discovery]
enabled = false
fifo_dir = "/var"
fifo_wait_timeout_secs = 300

# 以下两个字段仅在 gateway 侧发布时使用；node 启动本身不会消费。
# 如果为了与 gateway 配置片段保持一致，也可以按需显式补上。
# gateway_announce_retries = 5
# ip_intf = "eth0"
"#;

fn auto_discovery_value(doc: &toml::Value) -> Option<&toml::Value> {
    doc.get("gateway")
        .and_then(|gateway| gateway.get("node_control"))
        .and_then(|node_control| node_control.get("auto_discovery"))
}

fn secrets_value(doc: &toml::Value) -> Option<&toml::Value> {
    doc.get("secrets")
}

fn resolve_stored_gateway_token(
    secret_store: &SecretStore,
    stored_token: Option<&str>,
    encrypted_storage_enabled: bool,
) -> Result<(Option<String>, bool)> {
    let Some(stored_token) = stored_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return Ok((None, false));
    };

    if !encrypted_storage_enabled {
        if SecretStore::is_encrypted(stored_token) {
            return Ok((Some(secret_store.decrypt(stored_token)?), false));
        }

        return Ok((Some(stored_token.to_string()), false));
    }

    if SecretStore::needs_migration(stored_token) {
        let (token, migrated) = secret_store.decrypt_and_migrate(stored_token)?;
        return Ok((Some(token), migrated.is_some()));
    }

    if SecretStore::is_encrypted(stored_token) {
        return Ok((Some(secret_store.decrypt(stored_token)?), false));
    }

    Ok((Some(stored_token.to_string()), true))
}

/// 创建默认节点身份文件（生成随机密钥）
fn create_default_identity_file(display_name: Option<String>) -> NodeIdentityFile {
    let pub_bytes: [u8; 32] = rand::random();
    let priv_bytes: [u8; 64] = rand::random();

    NodeIdentityFile {
        device_id: format!("zeroclaw-node-{}", uuid::Uuid::new_v4()),
        public_key_b64: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            pub_bytes,
        ),
        private_key_b64: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            priv_bytes,
        ),
        display_name: display_name.or_else(|| Some(default_display_name())),
    }
}

/// 完整节点配置
///
/// 所有配置项统一在此结构下管理，包含：
/// - gateway: 网关配置
/// - event_destination: 事件目的地配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 网关配置
    pub gateway: GatewayConfig,
    /// 事件目的地配置
    pub event_destination: EventDestinationConfig,
    /// token 等敏感字段的本地存储策略
    #[serde(default)]
    pub secrets: SecretsConfig,
}

/// 节点配置管理器
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// .zeroclaw_node 子目录
    pub zeroclaw_node_dir: PathBuf,
    /// 配置文件路径
    pub config_path: PathBuf,
    /// 身份文件路径
    pub identity_path: PathBuf,
    /// 完整配置
    pub config: Config,
}

impl NodeConfig {
    /// 获取配置基础目录（环境变量或默认 ~）
    fn get_config_base_dir() -> PathBuf {
        std::env::var(ZEROCLAW_NODE_CONFIG_DIR_ENV)
            .ok()
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("~/.zeroclaw_node"))
    }

    /// 初始化配置文件和身份（如果不存在）
    pub fn init_config_file() -> Result<String> {
        let zeroclaw_node_dir = Self::get_config_base_dir().join(CONFIG_SUBDIR);
        let config_path = zeroclaw_node_dir.join(CONFIG_FILE);
        let identity_path = zeroclaw_node_dir.join("identity").join("device.json");

        // 如果配置文件已存在，返回提示
        if config_path.exists() {
            anyhow::bail!("配置文件已存在：{}", config_path.display());
        }

        // 创建目录
        std::fs::create_dir_all(&zeroclaw_node_dir)
            .with_context(|| format!("创建配置目录失败：{}", zeroclaw_node_dir.display()))?;
        std::fs::create_dir_all(identity_path.parent().unwrap()).with_context(|| {
            format!(
                "创建身份目录失败：{}",
                identity_path.parent().unwrap().display()
            )
        })?;

        // 创建默认配置文件
        std::fs::write(&config_path, DEFAULT_NODE_CONFIG)
            .with_context(|| format!("写入配置文件失败：{}", config_path.display()))?;

        // 创建默认身份文件（明文存储，生成随机密钥）
        let identity = create_default_identity_file(None);

        std::fs::write(&identity_path, serde_json::to_string_pretty(&identity)?)
            .with_context(|| format!("写入身份文件失败：{}", identity_path.display()))?;

        Ok(config_path.to_string_lossy().to_string())
    }

    /// 加载或初始化配置
    pub async fn load_or_init() -> Result<Self> {
        let zeroclaw_node_dir = Self::get_config_base_dir().join(CONFIG_SUBDIR);
        let config_path = zeroclaw_node_dir.join(CONFIG_FILE);
        let identity_path = zeroclaw_node_dir.join("identity").join("device.json");

        // 创建目录
        std::fs::create_dir_all(&zeroclaw_node_dir).ok();
        std::fs::create_dir_all(identity_path.parent().unwrap()).ok();

        // 加载配置文件
        let (config, should_refresh_token_storage) = if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("读取配置文件失败：{}", config_path.display()))?;
            let doc: toml::Value = toml::from_str(&content)
                .with_context(|| format!("解析配置文件失败：{}", config_path.display()))?;
            let secrets = SecretsConfig {
                encrypt: secrets_value(&doc)
                    .and_then(|secrets| secrets.get("encrypt"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or_else(|| SecretsConfig::default().encrypt),
            };
            let auto_discovery = auto_discovery_value(&doc);
            let auto_discovery_enabled = auto_discovery
                .and_then(|g| g.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let encrypt_gateway_token = secrets.encrypt && auto_discovery_enabled;
            let secret_store = SecretStore::new(&zeroclaw_node_dir, encrypt_gateway_token);

            // 读取 gateway 配置
            let host = doc
                .get("gateway")
                .and_then(|g| g.get("host"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let port = doc
                .get("gateway")
                .and_then(|g| g.get("port"))
                .and_then(|v| v.as_integer())
                .map(|p| p as u16)
                .unwrap_or(42617);
            let stored_token = doc
                .get("gateway")
                .and_then(|g| g.get("token"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let (token, should_refresh_token_storage) = resolve_stored_gateway_token(
                &secret_store,
                stored_token.as_deref(),
                encrypt_gateway_token,
            )?;

            // 读取 events 配置（可选）
            let events = doc
                .get("gateway")
                .and_then(|g| g.get("events"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                });

            let gateway = GatewayConfig {
                host,
                port,
                token,
                events,
                node_control: NodeControlConfig::default(),
            };

            // 读取 event_destination 配置
            let event_dest_port = doc
                .get("event_destination")
                .and_then(|g| g.get("port"))
                .and_then(|v| v.as_integer())
                .map(|p| p as u16)
                .unwrap_or(42618);
            let event_dest_path = doc
                .get("event_destination")
                .and_then(|g| g.get("path"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "/response".to_string());

            let event_destination = EventDestinationConfig {
                port: event_dest_port,
                path: event_dest_path,
            };

            let auto_discovery_fifo_dir = auto_discovery
                .and_then(|g| g.get("fifo_dir"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(default_fifo_dir);
            let auto_discovery_fifo_wait_timeout_secs = auto_discovery
                .and_then(|g| g.get("fifo_wait_timeout_secs"))
                .and_then(|v| v.as_integer())
                .map(|p| p as u64)
                .unwrap_or_else(default_fifo_wait_timeout_secs);
            let auto_discovery_gateway_announce_retries = auto_discovery
                .and_then(|g| g.get("gateway_announce_retries"))
                .and_then(|v| v.as_integer())
                .map(|p| p as u32);
            let auto_discovery_ip_intf = auto_discovery
                .and_then(|g| g.get("ip_intf"))
                .and_then(|v| v.as_str())
                .map(String::from);

            (
                Config {
                    gateway: GatewayConfig {
                        node_control: NodeControlConfig {
                            auto_discovery: AutoDiscoveryConfig {
                                enabled: auto_discovery_enabled,
                                fifo_dir: auto_discovery_fifo_dir,
                                fifo_wait_timeout_secs: auto_discovery_fifo_wait_timeout_secs,
                                gateway_announce_retries: auto_discovery_gateway_announce_retries,
                                ip_intf: auto_discovery_ip_intf,
                            },
                        },
                        ..gateway
                    },
                    event_destination,
                    secrets,
                },
                should_refresh_token_storage,
            )
        } else {
            // 配置文件不存在时创建默认配置
            std::fs::write(&config_path, DEFAULT_NODE_CONFIG)
                .with_context(|| format!("创建默认配置文件失败：{}", config_path.display()))?;
            (
                Config {
                    gateway: GatewayConfig {
                        host: "127.0.0.1".to_string(),
                        port: 42617,
                        token: None,
                        events: None,
                        node_control: NodeControlConfig::default(),
                    },
                    event_destination: EventDestinationConfig {
                        port: 42618,
                        path: "/response".to_string(),
                    },
                    secrets: SecretsConfig::default(),
                },
                false,
            )
        };

        let node_config = Self {
            zeroclaw_node_dir,
            config_path,
            identity_path,
            config,
        };

        if should_refresh_token_storage {
            node_config.persist_config()?;
        }

        Ok(node_config)
    }

    pub fn gateway_endpoint(&self) -> GatewayEndpoint {
        GatewayEndpoint {
            host: self.config.gateway.host.clone(),
            port: self.config.gateway.port,
            token: self.config.gateway.token.clone(),
        }
    }

    pub fn persist_discovered_gateway_endpoint(&mut self, gateway: &GatewayEndpoint) -> Result<()> {
        self.update_gateway_endpoint(gateway.clone())
    }

    fn persist_config(&self) -> Result<()> {
        let mut persisted_config = self.config.clone();
        let encrypt_gateway_token =
            self.config.secrets.encrypt && self.config.gateway.node_control.auto_discovery.enabled;
        let secret_store = SecretStore::new(&self.zeroclaw_node_dir, encrypt_gateway_token);

        persisted_config.gateway.token = persisted_config
            .gateway
            .token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(|token| {
                if encrypt_gateway_token {
                    secret_store.encrypt(token)
                } else {
                    Ok(token.to_string())
                }
            })
            .transpose()?;

        let content = toml::to_string_pretty(&persisted_config).context("序列化节点配置失败")?;
        std::fs::write(&self.config_path, content)
            .with_context(|| format!("写入配置文件失败：{}", self.config_path.display()))
    }

    pub fn update_gateway(&mut self, gateway: GatewayConfig) -> Result<()> {
        self.config.gateway = gateway;
        self.persist_config()
    }

    pub fn update_gateway_endpoint(&mut self, gateway: GatewayEndpoint) -> Result<()> {
        self.update_gateway(GatewayConfig {
            host: gateway.host,
            port: gateway.port,
            token: gateway.token,
            events: self.config.gateway.events.clone(),
            node_control: self.config.gateway.node_control.clone(),
        })
    }

    pub fn configure_gateway_interactively(&mut self) -> Result<()> {
        let gateway = interactive_gateway_config(&self.config.gateway)?;
        self.update_gateway(gateway)
    }

    pub fn event_destination_url_for_host(&self, host: &str) -> String {
        format!(
            "http://{}:{}{}",
            host, self.config.event_destination.port, self.config.event_destination.path
        )
    }

    pub fn resolve_local_event_destination_target(&self) -> (String, Option<String>) {
        let gateway_endpoint = self.gateway_endpoint();
        (
            self.event_destination_url_for_host(&gateway_endpoint.host),
            gateway_endpoint.token,
        )
    }

    pub fn resolve_business_event_destination_target(&self) -> Result<(String, Option<String>)> {
        Ok(self.resolve_local_event_destination_target())
    }

    /// 加载或创建节点身份
    pub fn load_or_create_identity(&self, display_name: Option<String>) -> Result<NodeIdentity> {
        let identity_context = ResolvedNodeIdentityContext {
            display_name: display_name.clone().unwrap_or_else(default_display_name),
            display_name_is_explicit: display_name.is_some(),
            gateway: self.gateway_endpoint(),
        };
        self.load_or_create_identity_with_context(&identity_context)
    }

    pub fn load_or_create_identity_profile(
        &self,
        context: &ResolvedNodeProfileContext,
    ) -> Result<NodeIdentity> {
        let path = &self.identity_path;

        let id_file = if path.exists() {
            let data = std::fs::read_to_string(path)?;
            let mut id_file: NodeIdentityFile = serde_json::from_str(&data)?;
            let mut should_persist = false;

            if id_file.display_name.is_none() || context.display_name_is_explicit {
                let display_name = Some(context.display_name.clone());
                if id_file.display_name != display_name {
                    id_file.display_name = display_name;
                    should_persist = true;
                }
            }

            if should_persist {
                std::fs::write(path, serde_json::to_string_pretty(&id_file)?)
                    .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
            }

            id_file
        } else {
            let id_file = create_default_identity_file(Some(context.display_name.clone()));
            std::fs::write(path, serde_json::to_string_pretty(&id_file)?)
                .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
            id_file
        };

        Ok(NodeIdentity {
            device_id: id_file.device_id,
            host: String::new(),
            port: 0,
            token: None,
            display_name: id_file.display_name,
            events: self.config.gateway.events.clone().unwrap_or_default(),
        })
    }

    pub fn load_or_create_identity_with_context(
        &self,
        context: &ResolvedNodeIdentityContext,
    ) -> Result<NodeIdentity> {
        let path = &self.identity_path;
        let gateway_endpoint = context.gateway.clone();

        let id_file = if path.exists() {
            // 加载现有身份
            let data = std::fs::read_to_string(path)?;
            let mut id_file: NodeIdentityFile = serde_json::from_str(&data)?;
            let mut should_persist = false;

            if id_file.display_name.is_none() || context.display_name_is_explicit {
                let display_name = Some(context.display_name.clone());
                if id_file.display_name != display_name {
                    id_file.display_name = display_name;
                    should_persist = true;
                }
            }

            if should_persist {
                std::fs::write(path, serde_json::to_string_pretty(&id_file)?)
                    .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
            }

            id_file
        } else {
            // 创建新身份（明文存储，生成随机密钥）
            let id_file = create_default_identity_file(Some(context.display_name.clone()));
            std::fs::write(path, serde_json::to_string_pretty(&id_file)?)
                .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
            id_file
        };

        // 统一构建 NodeIdentity
        Ok(NodeIdentity {
            device_id: id_file.device_id,
            host: gateway_endpoint.host,
            port: gateway_endpoint.port,
            token: gateway_endpoint.token,
            display_name: id_file.display_name,
            events: self.config.gateway.events.clone().unwrap_or_default(),
        })
    }
}

/// 交互式网关配置
pub fn interactive_gateway_config(current_gateway: &GatewayConfig) -> Result<GatewayConfig> {
    use dialoguer::{Input, Password};

    let host: String = Input::new()
        .with_prompt("网关主机")
        .default(current_gateway.host.clone())
        .interact_text()?;
    let port: u16 = Input::new()
        .with_prompt("网关端口")
        .default(current_gateway.port)
        .interact_text()?;

    let token_prompt = if current_gateway.token.is_some() {
        "网关 token (留空保持现有 token)"
    } else {
        "网关 token"
    };
    let token: String = Password::new()
        .with_prompt(token_prompt)
        .allow_empty_password(true)
        .interact()?;

    Ok(GatewayConfig {
        host,
        port,
        token: if token.trim().is_empty() {
            current_gateway.token.clone()
        } else {
            Some(token)
        },
        events: current_gateway.events.clone(),
        node_control: current_gateway.node_control.clone(),
    })
}

fn resolve_identity_context_from_defaults(
    default_host: String,
    default_port: u16,
    default_token: Option<String>,
    interactive: bool,
    host: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    token: Option<String>,
) -> Result<ResolvedNodeIdentityContext> {
    let display_name_is_explicit = name.is_some();
    let initial_host = host.unwrap_or(default_host);
    let initial_port = port.unwrap_or(default_port);
    let initial_token = token.or(default_token);

    let gateway = if interactive {
        use dialoguer::{Input, Password};

        let host: String = Input::new()
            .with_prompt("Gateway host")
            .default(initial_host)
            .interact_text()?;
        let port: u16 = Input::new()
            .with_prompt("Gateway port")
            .default(initial_port)
            .interact_text()?;
        let token_prompt = if initial_token.is_some() {
            "Gateway token (leave blank to keep current token)"
        } else {
            "Gateway token"
        };
        let token_input: String = Password::new()
            .with_prompt(token_prompt)
            .allow_empty_password(initial_token.is_some())
            .interact()?;
        let token = if token_input.trim().is_empty() {
            initial_token
        } else {
            Some(token_input)
        };

        GatewayEndpoint::new(host, port, token)
    } else {
        GatewayEndpoint::new(initial_host, initial_port, initial_token)
    };

    Ok(ResolvedNodeIdentityContext {
        display_name: name.unwrap_or_else(default_display_name),
        display_name_is_explicit,
        gateway,
    })
}

pub fn resolve_local_node_identity_context(
    config: &NodeConfig,
    interactive: bool,
    host: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    token: Option<String>,
) -> Result<ResolvedNodeIdentityContext> {
    let gateway = config.gateway_endpoint();
    resolve_identity_context_from_defaults(
        gateway.host,
        gateway.port,
        gateway.token,
        interactive,
        host,
        port,
        name,
        token,
    )
}

pub fn resolve_local_node_profile_context(name: Option<String>) -> ResolvedNodeProfileContext {
    let display_name_is_explicit = name.is_some();

    ResolvedNodeProfileContext {
        display_name: name.unwrap_or_else(default_display_name),
        display_name_is_explicit,
    }
}

pub fn resolve_local_auto_discovery_context(
    config: &NodeConfig,
    enabled: Option<bool>,
    fifo_dir: Option<String>,
    fifo_wait_timeout_secs: Option<u64>,
) -> ResolvedAutoDiscoveryContext {
    let base = &config.config.gateway.node_control.auto_discovery;

    ResolvedAutoDiscoveryContext {
        enabled: enabled.unwrap_or(base.enabled),
        fifo_dir: fifo_dir.unwrap_or_else(|| base.fifo_dir.clone()),
        fifo_wait_timeout_secs: fifo_wait_timeout_secs.unwrap_or(base.fifo_wait_timeout_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AutoDiscoveryConfig, Config, DEFAULT_NODE_CONFIG, EventDestinationConfig, GatewayConfig,
        GatewayEndpoint, NodeConfig, NodeControlConfig, ResolvedNodeProfileContext,
        resolve_local_auto_discovery_context, resolve_local_node_identity_context,
        resolve_stored_gateway_token,
    };
    use anyhow::Result;
    use tempfile::TempDir;
    use zeroclaw_config::{schema::SecretsConfig, secrets::SecretStore};

    fn test_node_config(tempdir: &TempDir) -> NodeConfig {
        let zeroclaw_node_dir = tempdir.path().join(".zeroclaw_node");
        let identity_dir = zeroclaw_node_dir.join("identity");
        std::fs::create_dir_all(&identity_dir).expect("create identity dir");

        NodeConfig {
            zeroclaw_node_dir,
            config_path: tempdir.path().join(".zeroclaw_node").join("config.toml"),
            identity_path: identity_dir.join("device.json"),
            config: Config {
                gateway: GatewayConfig {
                    host: "127.0.0.1".to_string(),
                    port: 42617,
                    token: None,
                    events: Some(vec!["router.alert".to_string()]),
                    node_control: NodeControlConfig {
                        auto_discovery: AutoDiscoveryConfig {
                            enabled: true,
                            fifo_dir: "/tmp/node-fifo".to_string(),
                            fifo_wait_timeout_secs: 123,
                            gateway_announce_retries: Some(5),
                            ip_intf: Some("utun0".to_string()),
                        },
                    },
                },
                event_destination: EventDestinationConfig {
                    port: 42618,
                    path: "/response".to_string(),
                },
                secrets: SecretsConfig { encrypt: false },
            },
        }
    }

    #[test]
    fn update_gateway_persists_and_identity_uses_latest_gateway() -> Result<()> {
        let tempdir = TempDir::new()?;
        let mut node_config = test_node_config(&tempdir);

        let initial_identity = node_config.load_or_create_identity(None)?;
        assert_eq!(initial_identity.host, "127.0.0.1");
        assert_eq!(initial_identity.port, 42617);
        assert_eq!(initial_identity.token, None);

        node_config.update_gateway(GatewayConfig {
            host: "10.0.0.9".to_string(),
            port: 43000,
            token: Some("node-secret".to_string()),
            events: Some(vec!["router.alert".to_string(), "router.warn".to_string()]),
            node_control: node_config.config.gateway.node_control.clone(),
        })?;

        let updated_identity = node_config.load_or_create_identity(None)?;
        let persisted_config = std::fs::read_to_string(&node_config.config_path)?;

        assert_eq!(updated_identity.device_id, initial_identity.device_id);
        assert_eq!(updated_identity.host, "10.0.0.9");
        assert_eq!(updated_identity.port, 43000);
        assert_eq!(updated_identity.token.as_deref(), Some("node-secret"));
        assert_eq!(
            updated_identity.events,
            vec!["router.alert".to_string(), "router.warn".to_string()]
        );
        assert!(persisted_config.contains("host = \"10.0.0.9\""));
        assert!(persisted_config.contains("port = 43000"));
        assert!(persisted_config.contains("token = \"node-secret\""));
        assert!(persisted_config.contains("[gateway.node_control.auto_discovery]"));
        assert!(persisted_config.contains("gateway_announce_retries = 5"));
        assert!(persisted_config.contains("ip_intf = \"utun0\""));

        Ok(())
    }

    #[test]
    fn default_template_keeps_gateway_only_fields_optional() {
        assert!(DEFAULT_NODE_CONFIG.contains("[secrets]"));
        assert!(DEFAULT_NODE_CONFIG.contains("encrypt = true"));
        assert!(DEFAULT_NODE_CONFIG.contains("[gateway.node_control.auto_discovery]"));
        assert!(DEFAULT_NODE_CONFIG.contains("enabled = false"));
        assert!(DEFAULT_NODE_CONFIG.contains("fifo_dir = \"/var\""));
        assert!(DEFAULT_NODE_CONFIG.contains("fifo_wait_timeout_secs = 300"));
        assert!(DEFAULT_NODE_CONFIG.contains("# gateway_announce_retries = 5"));
        assert!(DEFAULT_NODE_CONFIG.contains("# ip_intf = \"eth0\""));
        assert!(!DEFAULT_NODE_CONFIG.contains("\ngateway_announce_retries = "));
        assert!(!DEFAULT_NODE_CONFIG.contains("\nip_intf = "));
    }

    #[test]
    fn discovered_gateway_endpoint_updates_gateway_config() -> Result<()> {
        let tempdir = TempDir::new()?;
        let mut node_config = test_node_config(&tempdir);
        let gateway = GatewayEndpoint::new(
            "192.168.1.15".to_string(),
            45000,
            Some("updated-token".to_string()),
        );

        node_config.persist_discovered_gateway_endpoint(&gateway)?;

        assert_eq!(node_config.gateway_endpoint(), gateway);

        let persisted = std::fs::read_to_string(&node_config.config_path)?;
        assert!(persisted.contains("host = \"192.168.1.15\""));
        assert!(persisted.contains("port = 45000"));
        assert!(persisted.contains("token = \"updated-token\""));

        Ok(())
    }

    #[test]
    fn update_gateway_encrypts_token_when_secrets_enabled() -> Result<()> {
        let tempdir = TempDir::new()?;
        let mut node_config = test_node_config(&tempdir);
        node_config.config.secrets.encrypt = true;

        node_config.update_gateway_endpoint(GatewayEndpoint::new(
            "127.0.0.1".to_string(),
            42617,
            Some("zc_secret_token".to_string()),
        ))?;

        let persisted = std::fs::read_to_string(&node_config.config_path)?;
        let doc: toml::Value = toml::from_str(&persisted)?;
        let stored_token = doc
            .get("gateway")
            .and_then(|gateway| gateway.get("token"))
            .and_then(|value| value.as_str())
            .expect("stored token");

        assert!(stored_token.starts_with("enc2:"));

        let store = SecretStore::new(&node_config.zeroclaw_node_dir, true);
        assert_eq!(store.decrypt(stored_token)?, "zc_secret_token");
        assert_eq!(
            node_config.config.gateway.token.as_deref(),
            Some("zc_secret_token")
        );

        Ok(())
    }

    #[test]
    fn update_gateway_keeps_plaintext_in_static_mode_even_when_secrets_enabled() -> Result<()> {
        let tempdir = TempDir::new()?;
        let mut node_config = test_node_config(&tempdir);
        node_config.config.secrets.encrypt = true;
        node_config
            .config
            .gateway
            .node_control
            .auto_discovery
            .enabled = false;

        node_config.update_gateway_endpoint(GatewayEndpoint::new(
            "127.0.0.1".to_string(),
            42617,
            Some("zc_static_plaintext".to_string()),
        ))?;

        let persisted = std::fs::read_to_string(&node_config.config_path)?;
        assert!(persisted.contains("token = \"zc_static_plaintext\""));
        assert!(!persisted.contains("token = \"enc2:"));

        Ok(())
    }

    #[test]
    fn resolve_stored_gateway_token_requests_refresh_for_plaintext_when_encryption_enabled()
    -> Result<()> {
        let tempdir = TempDir::new()?;
        let store = SecretStore::new(tempdir.path(), true);

        let (token, should_refresh) =
            resolve_stored_gateway_token(&store, Some("zc_plaintext"), true)?;

        assert_eq!(token.as_deref(), Some("zc_plaintext"));
        assert!(should_refresh);

        Ok(())
    }

    #[test]
    fn resolve_stored_gateway_token_allows_encrypted_value_without_refresh_in_static_mode()
    -> Result<()> {
        let tempdir = TempDir::new()?;
        let store = SecretStore::new(tempdir.path(), true);
        let encrypted = store.encrypt("zc_plaintext")?;

        let (token, should_refresh) =
            resolve_stored_gateway_token(&store, Some(&encrypted), false)?;

        assert_eq!(token.as_deref(), Some("zc_plaintext"));
        assert!(!should_refresh);

        Ok(())
    }

    #[test]
    fn resolve_stored_gateway_token_keeps_static_plaintext_untouched() -> Result<()> {
        let tempdir = TempDir::new()?;
        let store = SecretStore::new(tempdir.path(), false);

        let (token, should_refresh) =
            resolve_stored_gateway_token(&store, Some("zc_static_plaintext"), false)?;

        assert_eq!(token.as_deref(), Some("zc_static_plaintext"));
        assert!(!should_refresh);

        Ok(())
    }

    #[test]
    fn load_or_create_identity_persists_display_name_override() -> Result<()> {
        let tempdir = TempDir::new()?;
        let node_config = test_node_config(&tempdir);

        node_config.load_or_create_identity(Some("alpha-node".to_string()))?;

        let persisted_identity = std::fs::read_to_string(&node_config.identity_path)?;
        assert!(persisted_identity.contains("\"display_name\": \"alpha-node\""));

        Ok(())
    }

    #[test]
    fn load_or_create_identity_profile_does_not_bind_local_gateway_endpoint() -> Result<()> {
        let tempdir = TempDir::new()?;
        let node_config = test_node_config(&tempdir);

        let identity =
            node_config.load_or_create_identity_profile(&ResolvedNodeProfileContext {
                display_name: "auto-node".to_string(),
                display_name_is_explicit: true,
            })?;

        assert_eq!(identity.host, "");
        assert_eq!(identity.port, 0);
        assert_eq!(identity.token, None);
        assert_eq!(identity.display_name.as_deref(), Some("auto-node"));

        Ok(())
    }

    #[test]
    fn resolve_local_node_identity_context_prefers_cli_then_local_defaults() -> Result<()> {
        let tempdir = TempDir::new()?;
        let node_config = test_node_config(&tempdir);

        let resolved = resolve_local_node_identity_context(
            &node_config,
            false,
            Some("cli-host".to_string()),
            None,
            Some("cli-node".to_string()),
            Some("cli-token".to_string()),
        )?;

        assert_eq!(resolved.display_name, "cli-node");
        assert!(resolved.display_name_is_explicit);
        assert_eq!(resolved.gateway.host, "cli-host");
        assert_eq!(resolved.gateway.port, 42617);
        assert_eq!(resolved.gateway.token.as_deref(), Some("cli-token"));

        Ok(())
    }

    #[test]
    fn resolve_local_auto_discovery_context_prefers_cli_then_local_defaults() {
        let tempdir = TempDir::new().expect("create tempdir");
        let node_config = test_node_config(&tempdir);

        let local_defaults = resolve_local_auto_discovery_context(&node_config, None, None, None);
        assert!(local_defaults.enabled);
        assert_eq!(local_defaults.fifo_dir, "/tmp/node-fifo");
        assert_eq!(local_defaults.fifo_wait_timeout_secs, 123);

        let resolved = resolve_local_auto_discovery_context(
            &node_config,
            Some(false),
            Some("/override/fifo".to_string()),
            Some(999),
        );
        assert!(!resolved.enabled);
        assert_eq!(resolved.fifo_dir, "/override/fifo");
        assert_eq!(resolved.fifo_wait_timeout_secs, 999);
    }

    #[test]
    fn resolve_business_event_destination_target_uses_local_gateway_settings() -> Result<()> {
        let tempdir = TempDir::new().expect("create tempdir");
        let mut node_config = test_node_config(&tempdir);
        node_config.update_gateway_endpoint(GatewayEndpoint::new(
            "10.1.2.3".to_string(),
            43000,
            Some("node-token".to_string()),
        ))?;

        let (url, token) = node_config.resolve_business_event_destination_target()?;
        assert_eq!(url, "http://10.1.2.3:42618/response");
        assert_eq!(token.as_deref(), Some("node-token"));

        Ok(())
    }
}
