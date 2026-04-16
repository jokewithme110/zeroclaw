use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::plannotebook::PlanNotebookEngine;
use zeroclaw_api::tool::{Tool, ToolResult};

pub struct PlanStatusTool {
    engine: Arc<Mutex<PlanNotebookEngine>>,
}

impl PlanStatusTool {
    pub fn new(engine: Arc<Mutex<PlanNotebookEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for PlanStatusTool {
    fn name(&self) -> &str {
        "plan_status"
    }

    fn description(&self) -> &str {
        "Get status for a plan run or plan definition."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Optional run ID to inspect"
                },
                "plan_id": {
                    "type": "string",
                    "description": "Optional plan ID to inspect"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let run_id = args.get("run_id").and_then(|v| v.as_str());
        let plan_id = args.get("plan_id").and_then(|v| v.as_str());

        let engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Plan engine lock poisoned: {e}"))?;

        let output = if let Some(run_id) = run_id {
            match engine.get_run(run_id) {
                Some(run) => serde_json::to_string_pretty(run)?,
                None => format!("Run not found: {run_id}"),
            }
        } else if let Some(plan_id) = plan_id {
            match engine.get_plan(plan_id) {
                Some(plan) => serde_json::to_string_pretty(plan)?,
                None => format!("Plan not found: {plan_id}"),
            }
        } else {
            let plans = engine.list_plans();
            if plans.is_empty() {
                "No plans found.".to_string()
            } else {
                let summary: Vec<serde_json::Value> = plans
                    .iter()
                    .map(|plan| {
                        json!({
                            "plan_id": plan.plan_id,
                            "goal": plan.goal,
                            "steps": plan.steps.len(),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&summary)?
            }
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}
