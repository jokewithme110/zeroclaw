//! Event subscriptions storage — tracks event subscriptions with channel and recipient.
//!
//! Stores information about event subscriptions including:
//! - Topic name
//! - Channel name (e.g., "qq", "feishu", "dingtalk")
//! - Recipient identifier (user/group/chat ID)
//! - Subscription timestamp
//!
//! Uses JSON file storage for simplicity and portability.

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An event subscription record
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EventSubscription {
    pub id: i64,
    pub topic: String,
    pub channel: String,
    pub recipient: String,
    pub subscribed_at: DateTime<Local>,
}

/// JSON file-backed event subscriptions store
#[derive(Clone)]
pub struct EventSubscriptionsStore {
    data: Arc<Mutex<EventStoreData>>,
    file_path: PathBuf,
}

/// The event store data structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EventStoreData {
    subscriptions: Vec<EventSubscription>,
    next_id: i64,
}

static EVENT_STORE_INSTANCE: OnceCell<EventSubscriptionsStore> = OnceCell::new();

impl EventSubscriptionsStore {
    /// Return the process-global store instance for the node runtime.
    pub fn global_instance(workspace_dir: &Path) -> Result<&'static Self> {
        EVENT_STORE_INSTANCE.get_or_try_init(|| Self::new(workspace_dir))
    }

    /// Create a new event subscriptions store
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let file_path = workspace_dir.join("events").join("subscriptions.json");

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Load existing data or create new
        let data = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .context("failed to read event subscriptions file")?;
            serde_json::from_str(&content).context("failed to parse event subscriptions file")?
        } else {
            EventStoreData::default()
        };

        let store = Self {
            data: Arc::new(Mutex::new(data)),
            file_path,
        };

        // Save initial state (creates file if new)
        store.save_to_file()?;

        Ok(store)
    }

    /// Save data to JSON file
    fn save_to_file(&self) -> Result<()> {
        let data = self.data.lock();
        let content = serde_json::to_string_pretty(&*data)
            .context("failed to serialize event subscriptions")?;
        drop(data); // Release lock before writing to file
        fs::write(&self.file_path, &content).context("failed to write event subscriptions file")?;
        Ok(())
    }

    /// Add or update an event subscription
    pub fn subscribe(&self, topic: &str, channel: &str, recipient: &str) -> Result<i64> {
        let now = Local::now();
        let id = {
            let mut data = self.data.lock();

            // Check if subscription already exists
            if let Some(sub) = data
                .subscriptions
                .iter_mut()
                .find(|s| s.topic == topic && s.channel == channel && s.recipient == recipient)
            {
                // Update existing subscription timestamp
                sub.subscribed_at = now;
                sub.id
            } else {
                // Create new subscription
                let id = data.next_id;
                data.next_id += 1;

                let subscription = EventSubscription {
                    id,
                    topic: topic.to_string(),
                    channel: channel.to_string(),
                    recipient: recipient.to_string(),
                    subscribed_at: now,
                };

                data.subscriptions.push(subscription);
                id
            }
        }; // Lock released here

        self.save_to_file()?;
        Ok(id)
    }

    /// Remove an event subscription
    pub fn unsubscribe(&self, topic: &str, channel: &str, recipient: &str) -> Result<bool> {
        let removed = {
            let mut data = self.data.lock();
            let initial_len = data.subscriptions.len();

            data.subscriptions.retain(|s| {
                !(s.topic == topic && s.channel == channel && s.recipient == recipient)
            });

            data.subscriptions.len() < initial_len
        }; // Lock released here

        if removed {
            self.save_to_file()?;
        }

        Ok(removed)
    }

    /// List all subscriptions, optionally filtered by topic, channel, or recipient
    pub fn list_subscriptions(
        &self,
        topic_filter: Option<&str>,
        channel_filter: Option<&str>,
        recipient_filter: Option<&str>,
    ) -> Result<Vec<EventSubscription>> {
        let data = self.data.lock();

        let mut result: Vec<EventSubscription> = data.subscriptions.clone();

        // Apply filters
        if let Some(topic) = topic_filter {
            result.retain(|s| s.topic == topic);
        }
        if let Some(channel) = channel_filter {
            result.retain(|s| s.channel == channel);
        }
        if let Some(recipient) = recipient_filter {
            result.retain(|s| s.recipient == recipient);
        }

        // Sort by subscribed_at descending
        result.sort_by_key(|subscription| Reverse(subscription.subscribed_at));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_subscribe_and_unsubscribe() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        // Subscribe
        let id = store.subscribe("alert.cpu", "qq", "user:123").unwrap();
        assert!(id >= 0);

        // List all
        let all = store.list_subscriptions(None, None, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].topic, "alert.cpu");
        assert_eq!(all[0].channel, "qq");
        assert_eq!(all[0].recipient, "user:123");

        // Unsubscribe
        let removed = store.unsubscribe("alert.cpu", "qq", "user:123").unwrap();
        assert!(removed);

        // Verify empty
        let after = store.list_subscriptions(None, None, None).unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn test_duplicate_subscribe_updates_timestamp() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        store
            .subscribe("alert.memory", "feishu", "chat:abc")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .subscribe("alert.memory", "feishu", "chat:abc")
            .unwrap();

        let all = store.list_subscriptions(None, None, None).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_filter_by_topic() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        store.subscribe("alert.cpu", "qq", "user:1").unwrap();
        store.subscribe("alert.cpu", "feishu", "user:2").unwrap();
        store.subscribe("alert.memory", "qq", "user:3").unwrap();

        let cpu_alerts = store
            .list_subscriptions(Some("alert.cpu"), None, None)
            .unwrap();
        assert_eq!(cpu_alerts.len(), 2);
    }

    #[test]
    fn test_filter_by_channel() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        store.subscribe("alert.cpu", "qq", "user:1").unwrap();
        store.subscribe("alert.cpu", "feishu", "user:2").unwrap();
        store.subscribe("alert.memory", "qq", "user:3").unwrap();

        let qq_alerts = store.list_subscriptions(None, Some("qq"), None).unwrap();
        assert_eq!(qq_alerts.len(), 2);
    }

    #[test]
    fn test_filter_by_recipient() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        store.subscribe("alert.cpu", "qq", "user:1").unwrap();
        store.subscribe("alert.cpu", "feishu", "user:1").unwrap();
        store.subscribe("alert.memory", "qq", "user:2").unwrap();

        let user1_alerts = store
            .list_subscriptions(None, None, Some("user:1"))
            .unwrap();
        assert_eq!(user1_alerts.len(), 2);
    }

    #[test]
    fn test_persistence() {
        let tmp = TempDir::new().unwrap();
        let store = EventSubscriptionsStore::new(tmp.path()).unwrap();

        store.subscribe("alert.cpu", "qq", "user:123").unwrap();
        store
            .subscribe("alert.memory", "feishu", "user:456")
            .unwrap();

        // Create new store instance (simulates restart)
        let store2 = EventSubscriptionsStore::new(tmp.path()).unwrap();
        let all = store2.list_subscriptions(None, None, None).unwrap();
        assert_eq!(all.len(), 2);
    }
}
