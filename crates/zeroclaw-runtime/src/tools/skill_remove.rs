//! `skill_remove` — agent-callable tool to remove a locally-installed skill.
//! Deletes the skill directory. Tools remain active until the agent process is restarted.

use crate::skills::{skill_dir_name, skills_dir, uninstall_local_skill};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};

pub struct SkillRemoveTool {
    workspace_dir: PathBuf,
}

impl SkillRemoveTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SkillRemoveTool {
    fn name(&self) -> &str {
        "skill_remove"
    }

    fn description(&self) -> &str {
        "Remove an installed skill. Deletes the skill directory. \
         Does not affect the remote SkillHub. \
         Restart the current agent process for tools to be fully removed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "SkillHub slug of the skill to remove, e.g. 'attendance-query-lite'."
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(slug) = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            // Argument validation reports a failed tool result (not an
            // `Err`) so the agent loop relays it back to the model as tool
            // output to self-correct, matching the not-installed/error paths
            // below.
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("missing or empty 'slug' parameter".to_string()),
            });
        };

        let skills_path = skills_dir(&self.workspace_dir);

        match uninstall_local_skill(&skills_path, slug) {
            Ok(true) => {
                let removed_path = skills_path.join(skill_dir_name(slug));
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Removed '{slug}' from {dir}. Restart the current agent process for changes to take effect.",
                        dir = removed_path.display(),
                    ),
                    error: None,
                })
            }
            Ok(false) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Skill '{slug}' is not installed")),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to remove '{slug}': {e}")),
            }),
        }
    }
}

impl Attributable for SkillRemoveTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Shell)
    }
    fn alias(&self) -> &str {
        "skill_remove"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir) -> SkillRemoveTool {
        SkillRemoveTool::new(tmp.path().to_path_buf())
    }

    #[tokio::test]
    async fn rejects_missing_slug() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp).execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("slug"));
    }

    #[tokio::test]
    async fn rejects_empty_slug() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp)
            .execute(json!({"slug": "  "}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn returns_error_for_not_installed_skill() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp)
            .execute(json!({"slug": "nonexistent-skill"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not installed"));
    }

    #[tokio::test]
    async fn removes_existing_skill() {
        let tmp = TempDir::new().unwrap();
        // Use a slug without hyphens so the on-disk name matches
        // skill_dir_name(slug). Hyphens are normalised to underscores,
        // so "test-skill" would be stored as "test_skill" on disk.
        let skill_dir = tmp.path().join("skills").join("testskill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Test\n").unwrap();

        let result = make_tool(&tmp)
            .execute(json!({"slug": "testskill"}))
            .await
            .unwrap();
        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("Removed 'testskill'"));
        assert!(!skill_dir.exists(), "skill dir should be deleted");
    }

    #[tokio::test]
    async fn rejects_path_traversal_in_slug() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp)
            .execute(json!({"slug": "../escape"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or("").contains("invalid")
                || result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("not installed")
        );
    }
}
