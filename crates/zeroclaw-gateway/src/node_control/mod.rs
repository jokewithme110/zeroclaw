//! Node control module for automatic gateway discovery and node connection.
//!
//! This module handles:
//! - `gateway.paired_tokens` recovery for node auto-discovery
//! - Gateway information publication via FIFO pipe to soft-bus
//! - Reuse of the primary paired token for node WebSocket authentication

pub mod fifo;
pub mod token;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use zeroclaw_config::schema::NodeControlConfig;
use zeroclaw_config::secrets::SecretStore;

pub use fifo::{GatewayAnnouncement, get_lan_ip, publish_gateway_info};
pub use token::PairedTokenInitialization;

/// Manages node control functionality including paired-token recovery and
/// gateway publication.
pub struct NodeControlManager {
    config: NodeControlConfig,
    stored_tokens: Vec<String>,
    config_path: PathBuf,
    secrets_encrypt_enabled: bool,
    secret_store: Arc<SecretStore>,
    current_token: RwLock<Option<String>>, // Plaintext in memory only
}

impl NodeControlManager {
    /// Create a new `NodeControlManager`.
    pub fn new(
        config: NodeControlConfig,
        stored_tokens: Vec<String>,
        config_path: PathBuf,
        secrets_encrypt_enabled: bool,
        secret_store: Arc<SecretStore>,
    ) -> Self {
        Self {
            config,
            stored_tokens,
            config_path,
            secrets_encrypt_enabled,
            secret_store,
            current_token: RwLock::new(None),
        }
    }

    /// Initialize or recover `gateway.paired_tokens` for node auto-discovery.
    pub async fn initialize_paired_tokens(&self) -> Result<PairedTokenInitialization> {
        if !self.config.auto_discovery.enabled {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip),
                "Node control auto-discovery disabled, skipping paired-token initialization"
            );
            return Ok(PairedTokenInitialization {
                paired_tokens: self.stored_tokens.clone(),
                primary_token: None,
                needs_persist: false,
            });
        }

        if !self.secrets_encrypt_enabled {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "event": "node_control_auto_discovery_requires_secret_encryption",
                    })),
                "Node auto-discovery is unavailable because secrets.encrypt must be true"
            );
            *self.current_token.write().await = None;
            return Ok(PairedTokenInitialization {
                paired_tokens: self.stored_tokens.clone(),
                primary_token: None,
                needs_persist: false,
            });
        }

        let stored_tokens = self
            .load_raw_paired_tokens()
            .unwrap_or_else(|| self.stored_tokens.clone());
        let resolved = token::resolve_paired_tokens(&self.secret_store, &stored_tokens);

        *self.current_token.write().await = resolved.primary_token.clone();

        Ok(resolved)
    }

    /// Get the current plaintext token used for discovery payloads.
    pub async fn get_token(&self) -> Option<String> {
        self.current_token.read().await.clone()
    }

    /// Publish gateway information to the FIFO pipe.
    ///
    /// If the FIFO pipe does not exist, it will be created automatically.
    pub async fn publish_gateway_info(&self, port: u16) -> Result<()> {
        if !self.is_auto_discovery_enabled() {
            return Ok(());
        }

        let token = self
            .get_token()
            .await
            .context("paired_tokens not initialized for auto-discovery")?;

        let lan_ip = get_lan_ip(self.config.auto_discovery.ip_intf.as_deref())
            .context("No LAN IP found for gateway announcement")?;

        fifo::publish_gateway_info(
            &self.config.auto_discovery.fifo_dir,
            &lan_ip,
            port,
            &token,
            self.config.auto_discovery.fifo_wait_timeout_secs,
            self.config.auto_discovery.gateway_announce_retries,
            true,
        )
        .await
    }

    /// Check if node control is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Check if auto-discovery is enabled.
    pub fn is_auto_discovery_enabled(&self) -> bool {
        self.config.auto_discovery.enabled && self.secrets_encrypt_enabled
    }

    fn load_raw_paired_tokens(&self) -> Option<Vec<String>> {
        let config_content = match std::fs::read_to_string(&self.config_path) {
            Ok(content) => content,
            Err(error) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read)
                        .with_attrs(::serde_json::json!({
                            "event": "node_control_raw_paired_tokens_read_failed",
                            "error": error.to_string(),
                            "path": self.config_path.display().to_string(),
                        })),
                    "Falling back to in-memory paired_tokens for auto-discovery initialization"
                );
                return None;
            }
        };

        let doc: toml::Value = match toml::from_str(&config_content) {
            Ok(doc) => doc,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "event": "node_control_raw_paired_tokens_parse_failed",
                            "error": error.to_string(),
                            "path": self.config_path.display().to_string(),
                        })),
                    "Failed to parse config.toml while recovering paired_tokens; falling back to in-memory config"
                );
                return None;
            }
        };

        doc.get("gateway")
            .and_then(|gateway| gateway.get("paired_tokens"))
            .and_then(toml::Value::as_array)
            .map(|tokens| {
                tokens
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn node_control_manager_creation() {
        let tmp = TempDir::new().unwrap();
        let secret_store = Arc::new(SecretStore::new(tmp.path(), true));
        let config = NodeControlConfig::default();

        let manager = NodeControlManager::new(
            config,
            Vec::new(),
            tmp.path().join("config.toml"),
            true,
            secret_store,
        );
        assert!(!manager.is_enabled());
        assert!(!manager.is_auto_discovery_enabled());
    }

    #[tokio::test]
    async fn initialize_paired_tokens_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let secret_store = Arc::new(SecretStore::new(tmp.path(), true));
        let mut config = NodeControlConfig::default();
        config.auto_discovery.enabled = false;

        let manager = NodeControlManager::new(
            config,
            vec!["zc_existing".into()],
            tmp.path().join("config.toml"),
            true,
            secret_store,
        );
        let result = manager.initialize_paired_tokens().await.unwrap();
        assert_eq!(result.paired_tokens, vec!["zc_existing"]);
        assert!(result.primary_token.is_none());
        assert!(!result.needs_persist);
    }

    #[tokio::test]
    async fn initialize_paired_tokens_when_enabled() {
        let tmp = TempDir::new().unwrap();
        let secret_store = Arc::new(SecretStore::new(tmp.path(), true));
        let mut config = NodeControlConfig::default();
        config.auto_discovery.enabled = true;

        let manager = NodeControlManager::new(
            config,
            Vec::new(),
            tmp.path().join("config.toml"),
            true,
            secret_store,
        );
        let result = manager.initialize_paired_tokens().await.unwrap();

        assert_eq!(result.paired_tokens.len(), 1);
        assert!(result.primary_token.as_deref().unwrap().starts_with("zc_"));
        assert!(result.needs_persist);
    }

    #[tokio::test]
    async fn initialize_reads_raw_paired_tokens_from_config_file() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[gateway]
paired_tokens = ["zc_raw_token"]
"#,
        )
        .unwrap();

        let secret_store = Arc::new(SecretStore::new(tmp.path(), true));
        let mut config = NodeControlConfig::default();
        config.auto_discovery.enabled = true;

        let manager = NodeControlManager::new(
            config,
            vec!["zc_in_memory_token".into()],
            config_path,
            true,
            secret_store,
        );
        let result = manager.initialize_paired_tokens().await.unwrap();

        assert_eq!(result.paired_tokens, vec!["zc_raw_token"]);
        assert_eq!(result.primary_token.as_deref(), Some("zc_raw_token"));
        assert!(!result.needs_persist);
    }

    #[tokio::test]
    async fn initialize_skips_auto_discovery_when_secret_encryption_disabled() {
        let tmp = TempDir::new().unwrap();
        let secret_store = Arc::new(SecretStore::new(tmp.path(), false));
        let mut config = NodeControlConfig::default();
        config.auto_discovery.enabled = true;

        let manager = NodeControlManager::new(
            config,
            vec!["zc_existing".into()],
            tmp.path().join("config.toml"),
            false,
            secret_store,
        );
        let result = manager.initialize_paired_tokens().await.unwrap();

        assert_eq!(result.paired_tokens, vec!["zc_existing"]);
        assert!(result.primary_token.is_none());
        assert!(!result.needs_persist);
        assert!(!manager.is_auto_discovery_enabled());
    }
}
