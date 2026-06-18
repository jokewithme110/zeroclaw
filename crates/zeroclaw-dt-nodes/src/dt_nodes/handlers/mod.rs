use serde_json::Value;

#[derive(Debug)]
pub struct InvokeOutcome {
    pub ok: bool,
    pub payload_json: Option<String>,
    pub error: Option<Value>,
}

pub trait Handler: Send + Sync {
    fn handle(&self, params_json: &str) -> InvokeOutcome;
}

pub mod camera_snap;
pub mod event_store;
pub mod event_subscribe;
pub mod event_subscribe_query;
pub mod event_unsubscribe;
pub mod file_save;
pub mod system_run;
