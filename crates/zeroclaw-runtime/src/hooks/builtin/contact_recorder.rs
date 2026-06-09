//! Contact recorder hook — records channel contacts when messages are received.
//!
//! Automatically tracks which channels/recipients have sent messages to the bot,
//! storing them in the channel contacts database for later reference.

use async_trait::async_trait;
use std::sync::Arc;

use zeroclaw_api::channel::ChannelMessage;

use crate::channel::contacts::ChannelContactsStore;
use crate::hooks::traits::{HookHandler, HookResult};

/// Hook that records channel contacts on message receive
pub struct ContactRecorderHook {
    store: Arc<ChannelContactsStore>,
}

impl ContactRecorderHook {
    pub fn new(store: Arc<ChannelContactsStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl HookHandler for ContactRecorderHook {
    fn name(&self) -> &str {
        "contact-recorder"
    }

    fn priority(&self) -> i32 {
        0
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        if message.channel == "webchat" {
            return HookResult::Continue(message);
        }

        // Record the contact (sender/reply_target as recipient)
        let recipient = if !message.reply_target.is_empty() {
            &message.reply_target
        } else {
            &message.sender
        };

        if let Err(e) = self.store.record_contact(&message.channel, recipient) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "hook": "contact-recorder",
                        "channel": message.channel,
                        "recipient": recipient,
                        "error": e.to_string()
                    })),
                "failed to record channel contact"
            );
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "hook": "contact-recorder",
                        "channel": message.channel,
                        "recipient": recipient
                    })),
                "recorded channel contact"
            );
        }

        HookResult::Continue(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_records_contact_on_message() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ChannelContactsStore::new(tmp.path()).unwrap());
        let hook = ContactRecorderHook::new(store.clone());

        let msg = ChannelMessage::new("msg-001", "user123", "user123", "hello", "qq", 1234567890);

        let result = hook.on_message_received(msg).await;
        assert!(!result.is_cancel());

        // Verify contact was recorded
        let contacts = store.list_contacts(None).unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].channel, "qq");
        assert_eq!(contacts[0].recipient, "user123");
    }

    #[tokio::test]
    async fn test_uses_reply_target_as_recipient() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ChannelContactsStore::new(tmp.path()).unwrap());
        let hook = ContactRecorderHook::new(store.clone());

        let msg = ChannelMessage {
            id: "msg-002".into(),
            sender: "actual_sender".into(),
            reply_target: "group123".into(),
            content: "hello group".into(),
            channel: "feishu".into(),
            timestamp: 1234567890,
            ..Default::default()
        };

        let _ = hook.on_message_received(msg).await;

        let contacts = store.list_contacts(None).unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].recipient, "group123");
    }
}
