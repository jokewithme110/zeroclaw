use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use anyhow::{Result, bail};

use super::types::{
    PlanNotebook, PlanRun, PlanRunAction, PlanRunState, PlanRunStatus, PlanStep, PlanStepResult,
    PlanStepStatus,
};

pub fn now_iso8601() -> String {
    // Use chrono if available, otherwise fallback to SystemTime
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Simple UTC timestamp without chrono dependency
    let secs = now.as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to Y-M-D (simplified — good enough for run IDs)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

pub struct PlanNotebookEngine {
    plans: HashMap<String, PlanNotebook>,
    active_runs: HashMap<String, PlanRun>,
    finished_runs: Vec<PlanRun>,
    state_dir: PathBuf,
    plan_counter: u64,
    run_counter: u64,
}

impl PlanNotebookEngine {
    pub fn new(workspace_dir: &Path) -> Self {
        let state_dir = workspace_dir.join("state").join("plannotebook");
        let _ = std::fs::create_dir_all(&state_dir);
        Self {
            plans: HashMap::new(),
            active_runs: HashMap::new(),
            finished_runs: Vec::new(),
            state_dir,
            plan_counter: 0,
            run_counter: 0,
        }
    }

    pub fn create_plan(&mut self, goal: String, mut steps: Vec<PlanStep>) -> Result<PlanNotebook> {
        if goal.trim().is_empty() {
            bail!("goal cannot be empty");
        }
        if steps.is_empty() {
            steps.push(PlanStep {
                number: 1,
                title: "Execute objective".to_string(),
                body: goal.clone(),
                acceptance_criteria: None,
                suggested_tools: Vec::new(),
            });
        } else {
            for (idx, step) in steps.iter_mut().enumerate() {
                step.number = u32::try_from(idx).unwrap_or(u32::MAX).saturating_add(1);
            }
        }

        self.plan_counter = self.plan_counter.saturating_add(1);
        let plan_id = format!("plan-{}-{:04}", now_epoch_ms(), self.plan_counter);
        let plan = PlanNotebook {
            plan_id: plan_id.clone(),
            goal,
            created_at: now_iso8601(),
            steps,
        };
        self.plans.insert(plan_id, plan.clone());
        Ok(plan)
    }

    pub fn list_plans(&self) -> Vec<PlanNotebook> {
        let mut plans: Vec<PlanNotebook> = self.plans.values().cloned().collect();
        plans.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
        plans
    }

    pub fn get_plan(&self, plan_id: &str) -> Option<&PlanNotebook> {
        self.plans.get(plan_id)
    }

    pub fn get_run(&self, run_id: &str) -> Option<&PlanRun> {
        self.active_runs
            .get(run_id)
            .or_else(|| self.finished_runs.iter().find(|r| r.run_id == run_id))
    }

    pub fn start_run(&mut self, plan_id: &str) -> Result<PlanRunAction> {
        let plan = self
            .plans
            .get(plan_id)
            .ok_or_else(|| anyhow::anyhow!("plan not found: {plan_id}"))?
            .clone();
        if plan.steps.is_empty() {
            bail!("plan has no steps: {plan_id}");
        }

        self.run_counter = self.run_counter.saturating_add(1);
        let run_id = format!("planrun-{}-{:04}", now_epoch_ms(), self.run_counter);
        let run = PlanRun {
            run_id: run_id.clone(),
            plan_id: plan_id.to_string(),
            status: PlanRunStatus::Running,
            current_step: 1,
            total_steps: u32::try_from(plan.steps.len()).unwrap_or(u32::MAX),
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
        };
        self.active_runs.insert(run_id.clone(), run);
        let step = plan.steps[0].clone();
        let context = format_step_context(&plan, 1, &step);
        self.persist_run_state(&run_id)?;
        Ok(PlanRunAction::ExecuteStep {
            run_id,
            step,
            context,
        })
    }

    pub fn advance_step(
        &mut self,
        run_id: &str,
        status: PlanStepStatus,
        output: String,
    ) -> Result<PlanRunAction> {
        let now = now_iso8601();
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::anyhow!("active run not found: {run_id}"))?;

        let current_step = run.current_step;
        let step_result = PlanStepResult {
            step_number: current_step,
            status,
            output,
            started_at: now.clone(),
            completed_at: Some(now),
        };
        run.step_results.push(step_result);

        if status == PlanStepStatus::Failed {
            let reason = format!("step {current_step} failed");
            return self.finish_run(run_id, PlanRunStatus::Failed, Some(reason));
        }

        let next_step = current_step.saturating_add(1);
        if next_step > run.total_steps {
            return self.finish_run(run_id, PlanRunStatus::Completed, None);
        }
        run.current_step = next_step;

        let plan = self
            .plans
            .get(&run.plan_id)
            .ok_or_else(|| anyhow::anyhow!("plan not found: {}", run.plan_id))?
            .clone();
        let idx = usize::try_from(next_step.saturating_sub(1)).unwrap_or(usize::MAX);
        let step = plan
            .steps
            .get(idx)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("step not found: {next_step}"))?;
        let context = format_step_context(&plan, next_step, &step);
        self.persist_run_state(run_id)?;
        Ok(PlanRunAction::ExecuteStep {
            run_id: run_id.to_string(),
            step,
            context,
        })
    }

    fn finish_run(
        &mut self,
        run_id: &str,
        status: PlanRunStatus,
        reason: Option<String>,
    ) -> Result<PlanRunAction> {
        let mut run = self
            .active_runs
            .remove(run_id)
            .ok_or_else(|| anyhow::anyhow!("active run not found: {run_id}"))?;
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let plan_id = run.plan_id.clone();
        self.persist_finished_run_state(&plan_id, &run)?;
        self.finished_runs.push(run.clone());
        let action = match status {
            PlanRunStatus::Completed => PlanRunAction::Completed {
                run_id: run_id.to_string(),
                plan_id,
            },
            PlanRunStatus::Failed => PlanRunAction::Failed {
                run_id: run_id.to_string(),
                plan_id,
                reason: reason.unwrap_or_else(|| "plan run failed".to_string()),
            },
            _ => PlanRunAction::Failed {
                run_id: run_id.to_string(),
                plan_id,
                reason: reason.unwrap_or_else(|| "plan run ended".to_string()),
            },
        };
        Ok(action)
    }

    fn persist_run_state(&self, run_id: &str) -> Result<()> {
        let run = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::anyhow!("active run not found: {run_id}"))?;
        self.persist_finished_run_state(&run.plan_id, run)
    }

    fn persist_finished_run_state(&self, plan_id: &str, run: &PlanRun) -> Result<()> {
        let plan = self
            .plans
            .get(plan_id)
            .ok_or_else(|| anyhow::anyhow!("plan not found: {plan_id}"))?;
        let state = PlanRunState {
            plan: plan.clone(),
            run: run.clone(),
        };
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::write(
            self.state_dir.join(format!("{}.state.json", run.run_id)),
            json,
        )?;
        Ok(())
    }

    pub fn load_run_state(path: &Path) -> Result<PlanRunState> {
        let raw = std::fs::read_to_string(path)?;
        let state: PlanRunState = serde_json::from_str(&raw)?;
        Ok(state)
    }
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719_468;
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn format_step_context(plan: &PlanNotebook, step_number: u32, step: &PlanStep) -> String {
    let mut context = String::new();
    let _ = writeln!(context, "Plan: {}", plan.goal);
    let _ = writeln!(
        context,
        "Step {step_number}/{}: {}",
        plan.steps.len(),
        step.title
    );
    if !step.body.is_empty() {
        let _ = writeln!(context, "Instructions: {}", step.body);
    }
    if let Some(criteria) = &step.acceptance_criteria {
        let _ = writeln!(context, "Acceptance criteria: {criteria}");
    }
    if !step.suggested_tools.is_empty() {
        let _ = writeln!(
            context,
            "Suggested tools: {}",
            step.suggested_tools.join(", ")
        );
    }
    context
}

fn now_epoch_ms() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_secs() * 1000 + u64::from(now.subsec_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_complete_plan_run() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let mut engine = PlanNotebookEngine::new(workspace.path());
        let plan = engine
            .create_plan(
                "Ship feature".to_string(),
                vec![
                    PlanStep {
                        number: 0,
                        title: "Implement".to_string(),
                        body: "Write code".to_string(),
                        acceptance_criteria: None,
                        suggested_tools: vec!["shell".to_string()],
                    },
                    PlanStep {
                        number: 0,
                        title: "Verify".to_string(),
                        body: "Run tests".to_string(),
                        acceptance_criteria: Some("All tests pass".to_string()),
                        suggested_tools: vec!["shell".to_string()],
                    },
                ],
            )
            .expect("create plan");
        let action1 = engine.start_run(&plan.plan_id).expect("start run");
        let run_id = match action1 {
            PlanRunAction::ExecuteStep { run_id, .. } => run_id,
            _ => panic!("expected execute step"),
        };

        let action2 = engine
            .advance_step(&run_id, PlanStepStatus::Completed, "done".to_string())
            .expect("advance to step2");
        assert!(matches!(action2, PlanRunAction::ExecuteStep { .. }));

        let action3 = engine
            .advance_step(&run_id, PlanStepStatus::Completed, "verified".to_string())
            .expect("complete run");
        assert!(matches!(action3, PlanRunAction::Completed { .. }));
    }

    #[test]
    fn failed_step_marks_run_failed() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let mut engine = PlanNotebookEngine::new(workspace.path());
        let plan = engine
            .create_plan("Test".to_string(), Vec::new())
            .expect("create plan");
        let action1 = engine.start_run(&plan.plan_id).expect("start run");
        let run_id = match action1 {
            PlanRunAction::ExecuteStep { run_id, .. } => run_id,
            _ => panic!("expected execute step"),
        };
        let action2 = engine
            .advance_step(&run_id, PlanStepStatus::Failed, "boom".to_string())
            .expect("fail run");
        assert!(matches!(action2, PlanRunAction::Failed { .. }));
    }
}
