//! zeroclaw-dt-nodes 配置模块
//!
//! 提供节点配置加载、mDNS 网关发现等功能
//!
//! 配置目录结构：
//! ```text
//! $ZEROCLAW_NODE_CONFIG_DIR/.zeroclaw_node/
//! ├── config.toml          # 配置文件
//! └── identity/
//!     └── device.json      # 节点身份（明文存储）
//! ```

mod node_config;

pub use node_config::{
    AutoDiscoveryConfig, GatewayEndpoint, NodeConfig, NodeIdentity, ResolvedAutoDiscoveryContext,
    ResolvedNodeIdentityContext, ResolvedNodeProfileContext, interactive_gateway_config,
    resolve_local_auto_discovery_context, resolve_local_node_identity_context,
    resolve_local_node_profile_context,
};
