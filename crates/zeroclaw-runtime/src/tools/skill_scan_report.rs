use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::SkillsScanConfig;
use zeroclaw_skill_security::client::SkillScanClient;
use zeroclaw_skill_security::store::SkillScanStore;

pub struct SkillScanReportTool {
    workspace_dir: PathBuf,
    enabled: bool,
    upload_url: String,
    result_url: String,
}

impl SkillScanReportTool {
    pub fn new(workspace_dir: PathBuf, scan_cfg: SkillsScanConfig) -> Self {
        Self {
            workspace_dir,
            enabled: scan_cfg.enabled,
            upload_url: scan_cfg.api.upload_url,
            result_url: scan_cfg.api.result_url,
        }
    }
}

#[async_trait]
impl Tool for SkillScanReportTool {
    fn name(&self) -> &str {
        "skill_scan_report"
    }

    fn description(&self) -> &str {
        "Query a skill's latest scan report by skill name. Reads the scan state task_no and fetches analysis_level, analysis_reason, and analysis_suggestion."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Skill directory name (skill_id) under workspace/skills."
                }
            },
            "required": ["skill_name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("skills.scan is disabled in config".to_string()),
            });
        }

        let skill_name = args
            .get("skill_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'skill_name' parameter"))?
            .to_string();

        let store = SkillScanStore::load(&self.workspace_dir)?;
        let Some(record) = store.get(&skill_name) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No scan state found for skill '{skill_name}'")),
            });
        };
        let Some(task_no) = record.scan_task_no.clone() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Skill '{skill_name}' has no task_no in scan state yet"
                )),
            });
        };

        let upload_url = self.upload_url.clone();
        let result_url = self.result_url.clone();
        let task_no_for_query = task_no.clone();
        let query = tokio::task::spawn_blocking(move || {
            let client = SkillScanClient::new(upload_url, result_url)?;
            client.query_result(&task_no_for_query)
        })
        .await
        .map_err(|err| anyhow::anyhow!("skill_scan_report join error: {err}"))??;

        let output = json!({
            "skill_name": skill_name,
            "task_no": task_no,
            "analysis_level": query.analysis_level,
            "analysis_reason": query.analysis_reason,
            "analysis_suggestion": query.analysis_suggestion
        });
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}
