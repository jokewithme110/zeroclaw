use crate::dt_nodes::handlers::event_store::EventSubscriptionsStore;
use crate::dt_nodes::handlers::{Handler, InvokeOutcome};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct EventSubscribeParams {
    #[serde(rename = "topics")]
    topics: Vec<String>,
    #[serde(rename = "channel")]
    channel: String,
    #[serde(rename = "recipient")]
    recipient: String,
}

#[derive(serde::Serialize)]
struct SubscribeResult {
    subscribed: bool,
    topics: Vec<String>,
    channel: String,
    recipient: String,
    count: usize,
}

pub struct EventSubscribeHandler {
    store: Option<EventSubscriptionsStore>,
    allowed_events: Vec<String>,
}

impl EventSubscribeHandler {
    pub fn new(store: Option<EventSubscriptionsStore>, allowed_events: Vec<String>) -> Self {
        Self {
            store,
            allowed_events,
        }
    }
}

impl Handler for EventSubscribeHandler {
    fn handle(&self, params_json: &str) -> InvokeOutcome {
        let trimmed = params_json.trim();
        if trimmed.is_empty() {
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(invalid_request("paramsJSON required")),
            };
        }

        let params: EventSubscribeParams = match serde_json::from_str(trimmed) {
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

        // 校验事件是否在允许的列表中
        if !self.allowed_events.is_empty() {
            for topic in &params.topics {
                if !self.allowed_events.contains(topic) {
                    return InvokeOutcome {
                        ok: false,
                        payload_json: None,
                        error: Some(invalid_request(&format!(
                            "event '{}' is not in the allowed events list",
                            topic
                        ))),
                    };
                }
            }
        }

        // Record subscriptions to database if store is available
        if let Some(ref store) = self.store {
            for topic in &params.topics {
                let _ = store.subscribe(topic, &params.channel, &params.recipient);
            }
        }

        let count = params.topics.len();
        let result = SubscribeResult {
            subscribed: true,
            topics: params.topics.clone(),
            channel: params.channel.clone(),
            recipient: params.recipient.clone(),
            count,
        };

        let payload = serde_json::json!({
            "subscribed": true,
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
    fn test_valid_subscribe() {
        let handler = EventSubscribeHandler::new(
            None,
            vec!["alert.cpu".to_string(), "alert.memory".to_string()],
        );
        let result = handler.handle(
            r#"{"topics": ["alert.cpu", "alert.memory"], "channel": "qq", "recipient": "user:123"}"#,
        );
        assert!(result.ok);
        assert!(result.error.is_none());
        assert!(result.payload_json.is_some());
    }

    #[test]
    fn test_empty_topics() {
        let handler = EventSubscribeHandler::new(None, vec![]);
        let result = handler.handle(r#"{"topics": [], "channel": "qq", "recipient": "user:123"}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_channel() {
        let handler = EventSubscribeHandler::new(None, vec![]);
        let result =
            handler.handle(r#"{"topics": ["alert.cpu"], "channel": "", "recipient": "user:123"}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_recipient() {
        let handler = EventSubscribeHandler::new(None, vec![]);
        let result =
            handler.handle(r#"{"topics": ["alert.cpu"], "channel": "qq", "recipient": ""}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_empty_params() {
        let handler = EventSubscribeHandler::new(None, vec![]);
        let result = handler.handle("");
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_event_not_allowed() {
        let handler = EventSubscribeHandler::new(None, vec!["alert.cpu".to_string()]);
        let result = handler
            .handle(r#"{"topics": ["alert.memory"], "channel": "qq", "recipient": "user:123"}"#);
        assert!(!result.ok);
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .to_string()
                .contains("not in the allowed events list")
        );
    }

    #[test]
    fn test_event_allowed() {
        let handler = EventSubscribeHandler::new(
            None,
            vec!["alert.cpu".to_string(), "alert.memory".to_string()],
        );
        let result = handler
            .handle(r#"{"topics": ["alert.cpu"], "channel": "qq", "recipient": "user:123"}"#);
        assert!(result.ok);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_no_allowed_events_constraint() {
        // 当 allowed_events 为空时，不校验限制
        let handler = EventSubscribeHandler::new(None, vec![]);
        let result = handler
            .handle(r#"{"topics": ["any.event"], "channel": "qq", "recipient": "user:123"}"#);
        // 此时应该通过 event 校验（因为 allowed_events 为空，不校验）
        // 但由于 topics 非空，应该继续执行
        // 实际上这个测试会因为没有 store 而跳过数据库操作，但仍然会成功
        assert!(result.ok);
    }
}
