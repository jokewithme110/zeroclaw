use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolResult};
use crate::plannotebook::{PlanNotebookEngine, PlanRunAction};

pub struct PlanStartTool {
    engine: Arc<Mutex<PlanNotebookEngine>>,
}

impl PlanStartTool {
    pub fn new(engine: Arc<Mutex<PlanNotebookEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for PlanStartTool {
    fn name(&self) -> &str {
        "plan_start"
    }

    fn description(&self) -> &str {
        "Start executing a previously created plan and return the first step context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "Plan ID returned by plan_create"
                }
            },
            "required": ["plan_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let plan_id = args
            .get("plan_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'plan_id' parameter"))?;

        let mut engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Plan engine lock poisoned: {e}"))?;
        let action = engine.start_run(plan_id)?;
        let output = match action {
            PlanRunAction::ExecuteStep {
                run_id, context, ..
            } => {
                format!("Plan run started: {run_id}\n\n{context}")
            }
            PlanRunAction::Completed { run_id, plan_id } => {
                format!("Plan '{plan_id}' run {run_id} completed immediately.")
            }
            PlanRunAction::Failed {
                run_id,
                plan_id,
                reason,
            } => {
                format!("Plan '{plan_id}' run {run_id} failed: {reason}")
            }
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}
