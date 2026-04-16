use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::plannotebook::{PlanNotebookEngine, PlanRunAction, PlanStepStatus};
use zeroclaw_api::tool::{Tool, ToolResult};

pub struct PlanStepUpdateTool {
    engine: Arc<Mutex<PlanNotebookEngine>>,
}

impl PlanStepUpdateTool {
    pub fn new(engine: Arc<Mutex<PlanNotebookEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for PlanStepUpdateTool {
    fn name(&self) -> &str {
        "plan_step_update"
    }

    fn description(&self) -> &str {
        "Record the result of the current plan step and advance the run."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Run ID from plan_start"
                },
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed", "skipped"],
                    "description": "Current step result status"
                },
                "output": {
                    "type": "string",
                    "description": "Brief output summary for this step"
                }
            },
            "required": ["run_id", "status", "output"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let run_id = args
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'run_id' parameter"))?;
        let status_str = args
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'status' parameter"))?;
        let output = args
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'output' parameter"))?
            .to_string();

        let status = match status_str {
            "completed" => PlanStepStatus::Completed,
            "failed" => PlanStepStatus::Failed,
            "skipped" => PlanStepStatus::Skipped,
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid status '{other}'. Must be completed, failed, or skipped"
                    )),
                });
            }
        };

        let mut engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Plan engine lock poisoned: {e}"))?;
        let action = engine.advance_step(run_id, status, output)?;

        let response = match action {
            PlanRunAction::ExecuteStep {
                run_id, context, ..
            } => {
                format!("Step recorded. Next step for run {run_id}:\n\n{context}")
            }
            PlanRunAction::Completed { run_id, plan_id } => {
                format!("Plan '{plan_id}' run {run_id} completed successfully.")
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
            output: response,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PlanCreateTool, PlanStartTool, PlanStatusTool};

    #[tokio::test]
    async fn plan_tools_execute_full_flow() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let engine = Arc::new(Mutex::new(crate::plannotebook::PlanNotebookEngine::new(
            workspace.path(),
        )));
        let create_tool = PlanCreateTool::new(Arc::clone(&engine));
        let start_tool = PlanStartTool::new(Arc::clone(&engine));
        let advance_tool = PlanStepUpdateTool::new(Arc::clone(&engine));
        let status_tool = PlanStatusTool::new(Arc::clone(&engine));

        let created = create_tool
            .execute(json!({
                "goal": "Deliver change",
                "steps": [
                    {"title": "Implement", "body": "Write code"},
                    {"title": "Validate", "body": "Run tests"}
                ]
            }))
            .await
            .expect("create call");
        assert!(created.success);
        let plan_id = created
            .output
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Plan created: "))
            .expect("plan id line");

        let started = start_tool
            .execute(json!({ "plan_id": plan_id }))
            .await
            .expect("start call");
        assert!(started.success);
        let run_id = started
            .output
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Plan run started: "))
            .expect("run id line");

        let advance_1 = advance_tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "done"
            }))
            .await
            .expect("advance step1");
        assert!(advance_1.success);
        assert!(advance_1.output.contains("Next step"));

        let advance_2 = advance_tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "verified"
            }))
            .await
            .expect("advance step2");
        assert!(advance_2.success);
        assert!(advance_2.output.contains("completed successfully"));

        let status = status_tool
            .execute(json!({ "run_id": run_id }))
            .await
            .expect("status call");
        assert!(status.success);
        assert!(status.output.contains("\"status\": \"completed\""));
    }
}
