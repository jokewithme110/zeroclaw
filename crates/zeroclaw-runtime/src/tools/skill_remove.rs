//! `skill_remove` — agent-callable tool to remove a locally-installed skill.
//! Deletes the skill directory. Tools remain active until /new.

use crate::skills::{skills_dir, uninstall_local_skill, uninstall_local_skill_by_name};
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
         Does not affect the remote SkillHub. Accepts either `slug` (the SkillHub directory name) or `name` (the manifest `name` field in SKILL.md). When both are provided, `slug` takes priority.Send /new for changes to take effect."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "SkillHub slug of the skill (the directory name), e.g. 'attendance-query-lite'. Takes priority when both slug and name are provided."
                },
                "name": {
                    "type": "string",
                    "description": "Manifest `name` field from the skill's SKILL.md, used as a fallback when the slug is not known. SkillHub is an open platform so this name can differ from the directory name."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if slug.is_none() && name.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "missing or empty: must provide either 'slug' or 'name' parameter".into(),
                ),
            });
        }

        let skills_path = skills_dir(&self.workspace_dir);

        // Slug takes priority: when provided, we can locate the skill
        // directly without scanning the workspace. `name` is the fallback
        // that handles the common case where the LLM only knows the
        // manifest `name` field, which SkillHub authors write freely.
        // `slug.is_none()` is guaranteed false here (gated above), so the
        // `else` arm can rely on `name` being `Some`.
        let result: Result<Option<String>, anyhow::Error> = if let Some(slug) = slug {
            uninstall_local_skill(&skills_path, slug).map(|removed| {
                if removed {
                    Some(slug.to_string())
                } else {
                    None
                }
            })
        } else {
            let name = name.expect("slug.is_none() && name.is_none() gated above");
            uninstall_local_skill_by_name(&skills_path, name)
        };

        match result {
            Ok(Some(slug)) => {
                let removed_path = skills_path.join(&slug);
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Removed '{slug}' from {dir}. Send /new for changes to take effect.",
                        slug = slug,
                        dir = removed_path.display(),
                    ),
                    error: None,
                })
            }
            Ok(None) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Skill is not installed".into()),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to remove skill: {e}")),
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
    async fn rejects_missing_slug_and_name() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp).execute(json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap_or("");
        assert!(
            err.contains("slug") && err.contains("name"),
            "error must mention both slug and name; got: {err}"
        );
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
    async fn returns_error_for_not_installed_name() {
        let tmp = TempDir::new().unwrap();
        let result = make_tool(&tmp)
            .execute(json!({"name": "nonexistent-name"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not installed"));
    }

    #[tokio::test]
    async fn removes_skill_by_manifest_name() {
        // The skill's directory is `skills/foo-bar/` but the manifest
        // declares `name: daily-news`. Caller (LLM) only knows the
        // manifest name and asks for removal by name — the tool should
        // still find and delete the right directory.
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("foo-bar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: daily-news\ndescription: daily news\n---\n",
        )
        .unwrap();

        let result = make_tool(&tmp)
            .execute(json!({"name": "daily-news"}))
            .await
            .unwrap();
        assert!(result.success, "got error: {:?}", result.error);
        assert!(
            result.output.contains("Removed 'foo-bar'"),
            "should report the directory slug it actually removed; got: {:?}",
            result.output
        );
        assert!(!skill_dir.exists(), "skill dir should be deleted");
    }

    #[tokio::test]
    async fn slug_takes_priority_over_name() {
        // Both are provided and identify different skills. The slug path
        // must win so a caller can disambiguate explicitly.
        let tmp = TempDir::new().unwrap();

        let by_slug = tmp.path().join("skills").join("slug-dir");
        std::fs::create_dir_all(&by_slug).unwrap();
        std::fs::write(by_slug.join("SKILL.md"), "---\nname: by-slug\n---\n").unwrap();

        let by_name = tmp.path().join("skills").join("name-dir");
        std::fs::create_dir_all(&by_name).unwrap();
        std::fs::write(by_name.join("SKILL.md"), "---\nname: by-name\n---\n").unwrap();

        let result = make_tool(&tmp)
            .execute(json!({"slug": "slug-dir", "name": "by-name"}))
            .await
            .unwrap();
        assert!(result.success, "got error: {:?}", result.error);
        assert!(
            result.output.contains("Removed 'slug-dir'"),
            "slug should win; got: {:?}",
            result.output
        );
        assert!(!by_slug.exists(), "slug-dir should be deleted");
        assert!(by_name.exists(), "name-dir should be untouched");
    }

    #[tokio::test]
    async fn removes_existing_skill() {
        let tmp = TempDir::new().unwrap();
        // The on-disk directory name matches the slug verbatim — a slug
        // with hyphens is stored with hyphens, so a SKILL.md command that
        // references `<slug>/...` resolves correctly.
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
    async fn removes_skill_with_hyphens() {
        // Regression: a slug like "device-health" used to be stored as
        // `device_health/`, so `skill_remove device-health` would silently
        // miss the directory and report "not installed". The skill dir
        // now matches the slug verbatim.
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("device-health");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Device Health\n").unwrap();

        let result = make_tool(&tmp)
            .execute(json!({"slug": "device-health"}))
            .await
            .unwrap();
        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("Removed 'device-health'"));
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
