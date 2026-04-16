use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolResult};
use crate::plannotebook::{PlanNotebookEngine, PlanStep};

pub struct PlanCreateTool {
    engine: Arc<Mutex<PlanNotebookEngine>>,
}

impl PlanCreateTool {
    pub fn new(engine: Arc<Mutex<PlanNotebookEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for PlanCreateTool {
    fn name(&self) -> &str {
        "plan_create"
    }

    fn description(&self) -> &str {
        "Create a structured execution plan from a goal. You can provide explicit steps or let the tool create a minimal single-step plan."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The objective this plan should accomplish"
                },
                "steps": {
                    "type": "array",
                    "description": "Optional explicit steps",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "body": {"type": "string"},
                            "acceptance_criteria": {"type": "string"},
                            "suggested_tools": {
                                "type": "array",
                                "items": {"type": "string"}
                            }
                        },
                        "required": ["title"]
                    }
                }
            },
            "required": ["goal"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let goal = args
            .get("goal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'goal' parameter"))?
            .to_string();
        let steps = parse_steps(args.get("steps"));

        let mut engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Plan engine lock poisoned: {e}"))?;
        let plan = engine.create_plan(goal, steps)?;

        let mut output = format!("Plan created: {}\nGoal: {}\n", plan.plan_id, plan.goal);
        output.push_str("Steps:\n");
        for step in &plan.steps {
            output.push_str(&format!("{}. {}\n", step.number, step.title));
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

fn parse_steps(raw_steps: Option<&serde_json::Value>) -> Vec<PlanStep> {
    let Some(serde_json::Value::Array(items)) = raw_steps else {
        return Vec::new();
    };
    let mut steps = Vec::new();
    for item in items {
        let Some(title) = item.get("title").and_then(|v| v.as_str()) else {
            continue;
        };
        let body = item
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let acceptance_criteria = item
            .get("acceptance_criteria")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let suggested_tools = item
            .get("suggested_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        steps.push(PlanStep {
            number: 0,
            title: title.to_string(),
            body,
            acceptance_criteria,
            suggested_tools,
        });
    }
    steps
}
