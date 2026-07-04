//! `skill_install` — agent-callable tool to download and install a skill
//! from the configured SkillHub. The skill is written to disk; tools become
//! available after /new.

use crate::skills::{
    install_skillhub_skill, resolve_skillhub_latest_version, skills_dir, validate_skill_slug,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;
use zeroclaw_config::skillhub::resolve_skillhub_base_url;

pub struct SkillInstallTool {
    config: Arc<Config>,
    workspace_dir: PathBuf,
    client: reqwest::Client,
}

impl SkillInstallTool {
    pub fn new(config: Arc<Config>, workspace_dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("zeroclaw-skillhub/0.8")
            .build()
            .expect("reqwest::Client builder should not fail with default config");
        Self {
            config,
            workspace_dir,
            client,
        }
    }
}

#[async_trait]
impl Tool for SkillInstallTool {
    fn name(&self) -> &str {
        "skill_install"
    }

    fn description(&self) -> &str {
        "Download and install a skill from the configured SkillHub. \
         Use skill_search first to find available skills. \
         If 'version' is omitted, the latest version is auto-resolved. \
         Send /new for new skill to take effect."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "SkillHub slug, e.g. 'attendance-query-lite'."
                },
                "version": {
                    "type": "string",
                    "description": "Optional version. Omit for latest. Format: yyyyMMdd.HHmmss (ICT) or semver."
                },
                "force": {
                    "type": "boolean",
                    "description": "If true, remove and reinstall when already installed. Default false."
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let slug = required_str(&args, "slug")?;
        if let Err(e) = validate_skill_slug(slug) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("{e}")),
            });
        }

        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        let base_url = resolve_skillhub_base_url(&self.config);
        let explicit_version = optional_str(&args, "version");

        let version = match explicit_version {
            Some(v) => v.to_string(),
            None => match resolve_skillhub_latest_version(&base_url, slug, &self.client).await {
                Ok(v) => v,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "failed to resolve latest version for '{slug}': {e}"
                        )),
                    });
                }
            },
        };

        let skills_path = skills_dir(&self.workspace_dir);
        let allow_scripts = self.config.skills.allow_scripts;

        let (dir, files_scanned) = match install_skillhub_skill(
            &base_url,
            slug,
            &version,
            &skills_path,
            allow_scripts,
            &self.client,
            force,
        )
        .await
        {
            Ok(pair) => pair,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("skill install failed: {e}")),
                });
            }
        };

        let installed_path = dir.display().to_string();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "slug": slug,
                    "version": version,
                    "base_url": base_url,
                    "installed_path": installed_path,
                    "files_scanned": files_scanned,
                })),
            "skill installed from SkillHub"
        );

        Ok(ToolResult {
            success: true,
            output: format!(
                "Installed {slug}@{version} to {installed_path} ({files_scanned} files scanned). Send /new for new skill to take effect."
            ),
            error: None,
        })
    }
}

fn required_str<'a>(args: &'a Value, name: &str) -> anyhow::Result<&'a str> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::Error::msg(format!("missing or empty '{name}' parameter")))
}

fn optional_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl Attributable for SkillInstallTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Shell)
    }
    fn alias(&self) -> &str {
        "skill_install"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_str_rejects_empty() {
        let args = json!({"slug": ""});
        assert!(required_str(&args, "slug").is_err());
    }

    #[test]
    fn required_str_rejects_missing() {
        let args = json!({});
        assert!(required_str(&args, "slug").is_err());
    }

    #[test]
    fn required_str_accepts_non_empty() {
        let args = json!({"slug": "attendance-query-lite"});
        assert_eq!(
            required_str(&args, "slug").unwrap(),
            "attendance-query-lite"
        );
    }

    #[test]
    fn optional_str_returns_none_for_missing() {
        let args = json!({"slug": "foo"});
        assert!(optional_str(&args, "version").is_none());
    }

    #[test]
    fn optional_str_treats_empty_as_none() {
        let args = json!({"slug": "foo", "version": "  "});
        assert!(optional_str(&args, "version").is_none());
    }
}
