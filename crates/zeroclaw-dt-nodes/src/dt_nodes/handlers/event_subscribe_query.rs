use crate::dt_nodes::handlers::event_store::EventSubscriptionsStore;
use crate::dt_nodes::handlers::{Handler, InvokeOutcome};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct EventSubscribeListParams {
    #[serde(rename = "channel")]
    channel: Option<String>,
}

#[derive(serde::Serialize)]
struct SubscriptionItem {
    pub id: i64,
    pub topic: String,
    pub channel: String,
    pub recipient: String,
    pub subscribed_at: String,
}

#[derive(serde::Serialize)]
struct SubscribeListResult {
    subscriptions: Vec<SubscriptionItem>,
    count: usize,
}

pub struct EventSubscribeListHandler {
    store: Option<EventSubscriptionsStore>,
}

impl EventSubscribeListHandler {
    pub fn new(store: Option<EventSubscriptionsStore>) -> Self {
        Self { store }
    }
}

impl Handler for EventSubscribeListHandler {
    fn handle(&self, params_json: &str) -> InvokeOutcome {
        let trimmed = params_json.trim();

        // Parse optional params
        let params: Option<EventSubscribeListParams> = if trimmed.is_empty() {
            None
        } else {
            match serde_json::from_str(trimmed) {
                Ok(p) => Some(p),
                Err(e) => {
                    return InvokeOutcome {
                        ok: false,
                        payload_json: None,
                        error: Some(invalid_request(&format!("invalid paramsJSON: {}", e))),
                    };
                }
            }
        };

        let channel_filter = params
            .and_then(|p| p.channel)
            .filter(|c| !c.trim().is_empty());

        // Query subscriptions from database if store is available
        let subscriptions = if let Some(ref store) = self.store {
            match store.list_subscriptions(None, channel_filter.as_deref(), None) {
                Ok(list) => list
                    .into_iter()
                    .map(|s| SubscriptionItem {
                        id: s.id,
                        topic: s.topic,
                        channel: s.channel,
                        recipient: s.recipient,
                        subscribed_at: s.subscribed_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    })
                    .collect(),
                Err(e) => {
                    return InvokeOutcome {
                        ok: false,
                        payload_json: None,
                        error: Some(internal_error(&format!(
                            "failed to query subscriptions: {}",
                            e
                        ))),
                    };
                }
            }
        } else {
            Vec::new()
        };

        let count = subscriptions.len();
        let result = SubscribeListResult {
            subscriptions,
            count,
        };

        let payload = serde_json::json!({
            "subscriptions": result.subscriptions,
            "count": result.count,
        });

        InvokeOutcome {
            ok: true,
            payload_json: Some(payload.to_string()),
            error: None,
        }
    }
}

fn invalid_request(msg: &str) -> Value {
    serde_json::json!({ "code": "INVALID_REQUEST", "message": msg })
}

fn internal_error(msg: &str) -> Value {
    serde_json::json!({ "code": "INTERNAL_ERROR", "message": msg })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_with_empty_params() {
        let handler = EventSubscribeListHandler::new(None);
        let result = handler.handle("");
        assert!(result.ok);
        assert!(result.error.is_none());
        assert!(result.payload_json.is_some());
        let payload: Value = serde_json::from_str(&result.payload_json.unwrap()).unwrap();
        assert_eq!(payload["count"], 0);
    }

    #[test]
    fn test_list_with_channel_filter() {
        let handler = EventSubscribeListHandler::new(None);
        let result = handler.handle(r#"{"channel": "qq"}"#);
        assert!(result.ok);
        assert!(result.error.is_none());
        assert!(result.payload_json.is_some());
    }

    #[test]
    fn test_list_with_invalid_json() {
        let handler = EventSubscribeListHandler::new(None);
        let result = handler.handle("{invalid json}");
        assert!(!result.ok);
        assert!(result.error.is_some());
    }
}
