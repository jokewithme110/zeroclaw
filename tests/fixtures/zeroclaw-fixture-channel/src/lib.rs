//! Channel plugin fixture for ZeroClaw.
//!
//! Demonstrates a dynamic plugin that registers a channel component.
//! Used to verify the channel plugin loading path end-to-end.

use async_trait::async_trait;
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_macros::plugin;

// ── The channel component ────────────────────────────────────────────────────

/// A simple test channel that echoes messages back.
pub struct FixtureEchoChannel {
    name: String,
}

impl FixtureEchoChannel {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl zeroclaw_api::attribution::Attributable for FixtureEchoChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Webhook)
    }
    fn alias(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl Channel for FixtureEchoChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        // Test channel: drop the message (no-op). A real channel would deliver it.
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // This is a test channel — it doesn't listen for real messages.
        // In a real implementation, this would connect to a messaging platform.
        // Keep the channel alive but don't send anything.
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    }

    async fn health_check(&self) -> bool {
        true
    }
}

// ── Factory function ─────────────────────────────────────────────────────────

/// Rust factory function — the `#[plugin]` macro wraps this into the C ABI
/// factory and exports the two required FFI symbols automatically.
#[plugin(channel = "fixture-echo-channel")]
fn fixture_echo_channel_factory(config: &str) -> Box<dyn Channel> {
    let name = if config.is_empty() {
        "fixture-echo-channel".to_string()
    } else {
        serde_json::from_str::<serde_json::Value>(config)
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
            .unwrap_or_else(|| "fixture-echo-channel".to_string())
    };
    Box::new(FixtureEchoChannel::new(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_export_matches_api_constant() {
        assert_eq!(zc_api_version(), zeroclaw_api::version::API_VERSION_U32);
    }

    #[test]
    fn factory_writes_non_null_box() {
        use core::ffi::c_void;
        let mut out: *mut c_void = core::ptr::null_mut();
        let rc = unsafe {
            __zc_factory_fixture_echo_channel_factory(
                core::ptr::null(),
                0,
                &mut out as *mut *mut c_void,
            )
        };
        assert_eq!(rc, 0);
        assert!(!out.is_null());
        // Reclaim ownership and drop to avoid leaking.
        let _: Box<Box<dyn Channel>> = unsafe { Box::from_raw(out as *mut Box<dyn Channel>) };
    }

    #[tokio::test]
    async fn channel_name_returns_expected() {
        let ch = FixtureEchoChannel::new("test-channel");
        assert_eq!(ch.name(), "test-channel");
    }

    #[tokio::test]
    async fn channel_health_check_returns_true() {
        let ch = FixtureEchoChannel::new("test-channel");
        assert!(ch.health_check().await);
    }

    #[tokio::test]
    async fn channel_send_does_not_fail() {
        let ch = FixtureEchoChannel::new("test-channel");
        let msg = SendMessage::new("hello", "test-recipient");
        ch.send(&msg).await.expect("send should not fail");
    }
}
