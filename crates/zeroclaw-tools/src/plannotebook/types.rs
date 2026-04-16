use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub number: u32,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
    #[serde(default)]
    pub suggested_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNotebook {
    pub plan_id: String,
    pub goal: String,
    pub created_at: String,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStepResult {
    pub step_number: u32,
    pub status: PlanStepStatus,
    pub output: String,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRun {
    pub run_id: String,
    pub plan_id: String,
    pub status: PlanRunStatus,
    pub current_step: u32,
    pub total_steps: u32,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub step_results: Vec<PlanStepResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanRunAction {
    ExecuteStep {
        run_id: String,
        step: PlanStep,
        context: String,
    },
    Completed {
        run_id: String,
        plan_id: String,
    },
    Failed {
        run_id: String,
        plan_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRunState {
    pub plan: PlanNotebook,
    pub run: PlanRun,
}
