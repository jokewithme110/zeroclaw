pub use zeroclaw_channels::orchestrator::*;
#[cfg(feature = "channel-matrix")]
pub mod matrix;
#[cfg(feature = "channel-telegram")]
pub mod telegram;
pub mod session_backend {
    pub use zeroclaw_infra::session_backend::*;
}
pub mod session_sqlite {
    pub use zeroclaw_infra::session_sqlite::*;
}

use crate::ContactsCommands;
use crate::config::Config;
use anyhow::{Context, Result};
#[cfg(feature = "channel-wechat")]
use zeroclaw_channels::wechat_binding::load_wechat_binding_status;
use zeroclaw_runtime::i18n::get_required_cli_string;
#[cfg(feature = "channel-notion")]
use zeroclaw_runtime::i18n::get_required_cli_string_with_args;

pub async fn handle_command(command: crate::ChannelCommands, config: &Config) -> Result<()> {
    match command {
        crate::ChannelCommands::Start => {
            anyhow::bail!("Start must be handled in main.rs (requires async runtime)")
        }
        crate::ChannelCommands::Doctor => {
            anyhow::bail!("Doctor must be handled in main.rs (requires async runtime)")
        }
        crate::ChannelCommands::List => {
            println!("{}", get_required_cli_string("cli-channels-header"));
            println!("{}", get_required_cli_string("cli-channels-cli-always"));
            for entry in zeroclaw_channels::listing::compiled_channels(&config.channels) {
                println!(
                    "  {} {}",
                    if entry.configured { "✅" } else { "❌" },
                    entry.name
                );
            }
            // Notion is a top-level config section, not part of ChannelsConfig
            #[cfg(feature = "channel-notion")]
            {
                let notion_configured =
                    config.notion.enabled && !config.notion.database_id.trim().is_empty();
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-channels-notion",
                        &[("status", if notion_configured { "✅" } else { "❌" })],
                    )
                );
            }
            println!();
            println!("{}", get_required_cli_string("cli-channels-start-hint"));
            println!("{}", get_required_cli_string("cli-channels-doctor-hint"));
            println!("{}", get_required_cli_string("cli-channels-configure-hint"));
            Ok(())
        }
        crate::ChannelCommands::Add {
            channel_type,
            config: _,
        } => {
            anyhow::bail!(
                "Channel type '{channel_type}' — use `zeroclaw config set channels.{channel_type}.<alias>.<field>=<value>` to configure"
            );
        }
        crate::ChannelCommands::Remove { name } => {
            anyhow::bail!("Remove channel '{name}' — edit ~/.zeroclaw/config.toml directly");
        }
        crate::ChannelCommands::BindTelegram { identity } => {
            Box::pin(bind_telegram_identity(config, &identity)).await
        }
        #[cfg(feature = "channel-wechat")]
        crate::ChannelCommands::WechatBindingStatus => wechat_binding_status(config),
        #[cfg(feature = "channel-wechat")]
        crate::ChannelCommands::WechatAuthorizeQr { timeout_ms } => {
            wechat_authorize_qr(config, timeout_ms).await
        }
        crate::ChannelCommands::Send {
            message,
            channel_id,
            recipient,
        } => send_channel_message(config, &channel_id, &recipient, &message).await,
        crate::ChannelCommands::Contacts { contacts_command } => {
            handle_contacts_command(contacts_command, config)
        }
    }
}

#[cfg(feature = "channel-wechat")]
fn wechat_binding_status(config: &Config) -> Result<()> {
    // Try to fetch from gateway first (if dashboard is available)
    match fetch_local_gateway_wechat_binding_status(config) {
        Ok(Some(json_output)) => {
            println!("{json_output}");
            return Ok(());
        }
        Ok(None) => {} // Gateway not available or endpoint not found, fall through
        Err(_) => {}   // Gateway error, fall through to direct file read
    }

    // Fallback: directly read from WeChat state files
    // For backward compatibility, use the first WeChat config if any exists
    let wechat_config = config.channels.wechat.values().next();
    let status = load_wechat_binding_status(wechat_config);
    let json_output = serde_json::to_string_pretty(&status)
        .context("Failed to serialize WeChat binding status")?;
    println!("{json_output}");
    Ok(())
}

#[cfg(feature = "channel-wechat")]
async fn wechat_authorize_qr(config: &Config, timeout_ms: Option<u64>) -> Result<()> {
    let json_output = fetch_local_gateway_wechat_authorize_qr(config, timeout_ms).await?;
    println!("{json_output}");
    Ok(())
}

#[cfg(feature = "channel-wechat")]
fn gateway_api_url(config: &Config, api_path: &str) -> String {
    let prefix = config.gateway.path_prefix.as_deref().unwrap_or("");
    format!("http://127.0.0.1:{}{prefix}{api_path}", config.gateway.port)
}

#[cfg(feature = "channel-wechat")]
fn fetch_local_gateway_wechat_binding_status(config: &Config) -> Result<Option<String>> {
    // Try API endpoint first (works without dashboard)
    let url = gateway_api_url(config, "/api/channels/wechat/binding-status");
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send();

    let response = match response {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("local gateway binding-status request failed ({status}): {body}");
    }

    let json: serde_json::Value = response
        .json()
        .context("Failed to parse local gateway WeChat binding-status response")?;
    serde_json::to_string_pretty(&json)
        .context("Failed to serialize local gateway WeChat binding-status response")
        .map(Some)
}

#[cfg(feature = "channel-wechat")]
async fn fetch_local_gateway_wechat_authorize_qr(
    config: &Config,
    timeout_ms: Option<u64>,
) -> Result<String> {
    let url = if let Some(timeout_ms) = timeout_ms {
        format!(
            "{}?timeout_ms={timeout_ms}",
            gateway_api_url(config, "/api/channels/wechat/authorize-qr")
        )
    } else {
        gateway_api_url(config, "/api/channels/wechat/authorize-qr")
    };

    let response = reqwest::Client::new()
        .post(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("Failed to connect to local gateway at {url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("local gateway authorize-qr request failed ({status}): {body}");
    }

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse local gateway WeChat authorize-qr response")?;

    serde_json::to_string_pretty(&json)
        .context("Failed to serialize local gateway WeChat authorize-qr response")
}

fn handle_contacts_command(command: ContactsCommands, config: &Config) -> Result<()> {
    match command {
        ContactsCommands::List { channel, json } => {
            use zeroclaw_runtime::channel::contacts::ChannelContactsStore;

            let store = match ChannelContactsStore::new(&config.data_dir) {
                Ok(s) => s,
                Err(e) => {
                    anyhow::bail!("Failed to open channel contacts store: {e}");
                }
            };

            let contacts = match store.list_contacts(channel.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    anyhow::bail!("Failed to list contacts: {e}");
                }
            };

            if contacts.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("No contacts found.");
                }
                return Ok(());
            }

            if json {
                // Output as JSON
                let json_output = serde_json::to_string_pretty(&contacts)
                    .context("Failed to serialize contacts to JSON")?;
                println!("{}", json_output);
            } else {
                // Output as table (default, sorted by last_seen desc)
                println!("{:<10} {:<35} Last Seen", "Channel", "Recipient");
                println!("{:-<10} {:-<35} {:-<20}", "", "", "");

                for contact in contacts {
                    println!(
                        "{:<10} {:<35} {}",
                        contact.channel,
                        contact.recipient,
                        contact.last_seen.format("%Y-%m-%d %H:%M")
                    );
                }
            }

            Ok(())
        }
    }
}
