//! Channel contacts storage — tracks channel recipients and last seen timestamps.
//!
//! Stores information about channels that have received messages, including:
//! - Channel type (qq, feishu, dingtalk, etc.)
//! - Recipient identifier (user/group/chat ID)
//! - Last seen timestamp

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A channel contact record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelContact {
    pub channel: String,
    pub recipient: String,
    pub last_seen: DateTime<Local>,
}

/// Internal JSON-serializable contact record
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContactRecord {
    channel: String,
    recipient: String,
    last_seen: String, // ISO 8601 format
}

/// JSON file-backed channel contacts storage
pub struct ChannelContactsStore {
    file_path: PathBuf,
    contacts: Arc<Mutex<Vec<ContactRecord>>>,
}

impl ChannelContactsStore {
    /// Create a new channel contacts store
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let file_path = workspace_dir.join("channels").join("contacts.json");

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contacts = Self::load_contacts(&file_path)?;

        Ok(Self {
            file_path,
            contacts: Arc::new(Mutex::new(contacts)),
        })
    }

    /// Load contacts from JSON file or return empty vec if file doesn't exist
    fn load_contacts(file_path: &Path) -> Result<Vec<ContactRecord>> {
        if !file_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(file_path).context("failed to read contacts file")?;

        if content.trim().is_empty() {
            return Ok(Vec::new());
        }

        let records: Vec<ContactRecord> =
            serde_json::from_str(&content).context("failed to parse contacts JSON")?;

        Ok(records)
    }

    /// Save contacts to JSON file
    fn save_contacts(&self, contacts: &[ContactRecord]) -> Result<()> {
        let json =
            serde_json::to_string_pretty(contacts).context("failed to serialize contacts")?;

        fs::write(&self.file_path, json).context("failed to write contacts file")?;

        Ok(())
    }

    /// Record or update a channel contact
    pub fn record_contact(&self, channel: &str, recipient: &str) -> Result<()> {
        let now = Local::now().to_rfc3339();

        let mut contacts = self.contacts.lock();

        // Check if contact exists and update, or add new
        if let Some(existing) = contacts
            .iter_mut()
            .find(|c| c.channel == channel && c.recipient == recipient)
        {
            existing.last_seen = now;
        } else {
            contacts.push(ContactRecord {
                channel: channel.to_string(),
                recipient: recipient.to_string(),
                last_seen: now,
            });
        }

        // Persist to disk
        self.save_contacts(&contacts)?;

        Ok(())
    }

    /// List all contacts, optionally filtered by channel
    pub fn list_contacts(&self, channel_filter: Option<&str>) -> Result<Vec<ChannelContact>> {
        let contacts = self.contacts.lock();

        let mut result: Vec<ChannelContact> = contacts
            .iter()
            .filter(|c| channel_filter.is_none_or(|filter| c.channel == filter))
            .filter_map(|record| {
                DateTime::parse_from_rfc3339(&record.last_seen)
                    .ok()
                    .map(|dt| ChannelContact {
                        channel: record.channel.clone(),
                        recipient: record.recipient.clone(),
                        last_seen: dt.with_timezone(&Local),
                    })
            })
            .collect();

        // Sort by last_seen descending
        result.sort_by_key(|b| std::cmp::Reverse(b.last_seen));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_record_and_list() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelContactsStore::new(tmp.path()).unwrap();

        store.record_contact("qq", "user:A1B2C3").unwrap();
        store.record_contact("qq", "group:G1H2I3").unwrap();
        store.record_contact("feishu", "oc_chat123").unwrap();

        let all = store.list_contacts(None).unwrap();
        assert_eq!(all.len(), 3);

        let qq_only = store.list_contacts(Some("qq")).unwrap();
        assert_eq!(qq_only.len(), 2);
        assert!(qq_only.iter().all(|c| c.channel == "qq"));
    }
}
