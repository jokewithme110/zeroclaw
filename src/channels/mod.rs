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
