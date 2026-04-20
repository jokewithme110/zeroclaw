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
pub mod file_save;
pub mod system_run;
