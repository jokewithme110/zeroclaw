use crate::dt_nodes::handlers::event_store::EventSubscriptionsStore;
use crate::dt_nodes::handlers::{Handler, InvokeOutcome};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct EventUnsubscribeParams {
    #[serde(rename = "topics")]
    topics: Vec<String>,
    #[serde(rename = "channel")]
    channel: String,
    #[serde(rename = "recipient")]
    recipient: String,
}

#[derive(serde::Serialize)]
struct UnsubscribeResult {
    unsubscribed: bool,
    topics: Vec<String>,
    channel: String,
    recipient: String,
    count: usize,
}

pub struct EventUnsubscribeHandler {
    store: Option<EventSubscriptionsStore>,
}

impl EventUnsubscribeHandler {
    pub fn new(store: Option<EventSubscriptionsStore>) -> Self {
        Self { store }
    }
}

impl Handler for EventUnsubscribeHandler {
    fn handle(&self, params_json: &str) -> InvokeOutcome {
        let trimmed = params_json.trim();
        if trimmed.is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("paramsJSON required")),
            };
        }

        let params: EventUnsubscribeParams = match serde_json::from_str(trimmed) {
            Ok(p) => p,
            Err(e) => {
                return InvokeOutcome {
                    ok: false,
                    payload_json: None,
                    error: Some(invalid_request(&format!("invalid paramsJSON: {}", e))),
                };
            }
        };

        if params.topics.is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("topics array required, must not be empty")),
            };
        }

        if params.channel.trim().is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("channel is required and must not be empty")),
            };
        }

        if params.recipient.trim().is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request(
                    "recipient is required and must not be empty",
                )),
            };
        }

        // Remove subscriptions from database if store is available
        if let Some(ref store) = self.store {
            for topic in &params.topics {
                let _ = store.unsubscribe(topic, &params.channel, &params.recipient);
            }
        }

        let count = params.topics.len();
        let result = UnsubscribeResult {
            unsubscribed: true,
            topics: params.topics.clone(),
            channel: params.channel.clone(),
            recipient: params.recipient.clone(),
            count,
        };

        let payload = serde_json::json!({
            "unsubscribed": true,
            "topics": result.topics,
            "channel": result.channel,
            "recipient": result.recipient,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_unsubscribe() {
        let handler = EventUnsubscribeHandler::new(None);
        let result = handler.handle(
            r#"{"topics": ["alert.cpu", "alert.memory"], "channel": "qq", "recipient": "user:123"}"#,
        );
        assert!(result.ok);
        assert!(result.error.is_none());
        assert!(result.payload_json.is_some());
    }

    #[test]
    fn test_empty_topics() {
        let handler = EventUnsubscribeHandler::new(None);
        let result = handler.handle(r#"{"topics": [], "channel": "qq", "recipient": "user:123"}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_channel() {
        let handler = EventUnsubscribeHandler::new(None);
        let result =
            handler.handle(r#"{"topics": ["alert.cpu"], "channel": "", "recipient": "user:123"}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_recipient() {
        let handler = EventUnsubscribeHandler::new(None);
        let result =
            handler.handle(r#"{"topics": ["alert.cpu"], "channel": "qq", "recipient": ""}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_params() {
        let handler = EventUnsubscribeHandler::new(None);
        let result = handler.handle("");
        assert!(!result.ok);
        assert!(result.error.is_some());
    }
}
