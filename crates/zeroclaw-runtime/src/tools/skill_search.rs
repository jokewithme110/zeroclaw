//! `skill_search` — agent-callable tool to discover skills on the configured
//! SkillHub. Uses the same HTTP client and serde models as the rest of the
//! SkillHub integration; see `skillhub_client` for response shapes.

use crate::tools::skillhub_client::{
    SkillListItem, SkillListResponse, SkillSearchItem, SkillSearchResponse,
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

pub struct SkillSearchTool {
    config: Arc<Config>,
    workspace_dir: PathBuf,
    client: reqwest::Client,
}

impl SkillSearchTool {
    pub fn new(config: Arc<Config>, workspace_dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
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
impl Tool for SkillSearchTool {
    fn name(&self) -> &str {
        "skill_search"
    }

    fn description(&self) -> &str {
        "Search for available skills on the configured SkillHub, \
         or list locally installed skills. \
         Pass a query to search remote; pass local=true to list installed; \
         omit both to list all remote skills. \
         Returns skill slug, name, version, description, and download count."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keyword. Omit or pass empty string to list all remote skills."
                },
                "local": {
                    "type": "boolean",
                    "description": "If true, list locally-installed skills (ignores query). Default false."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let local = args.get("local").and_then(|v| v.as_bool()).unwrap_or(false);
        if local {
            return self.list_local_skills();
        }

        let base = resolve_skillhub_base_url(&self.config);
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let url = match query {
            Some(q) => format!("{}/api/v1/search?q={}", base, urlencoding::encode(q)),
            None => format!("{}/api/v1/skills", base),
        };

        let resp =
            self.client.get(&url).send().await.map_err(|e| {
                anyhow::Error::msg(format!("SkillHub request to {url} failed: {e}"))
            })?;

        if !resp.status().is_success() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "SkillHub HTTP {} from {url} (WAF may be blocking the request)",
                    resp.status()
                )),
            });
        }

        // Branch on response shape: list endpoint returns `{items: [...]}`
        // while search returns `{results: [...]}` (see skillhub-api.md §5).
        let body = resp.text().await.map_err(|e| {
            anyhow::Error::msg(format!("failed to read SkillHub response body: {e}"))
        })?;

        let output = match query.is_some() {
            true => match serde_json::from_str::<SkillSearchResponse>(&body) {
                Ok(r) => format_skill_search_results(&r.results),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("SkillHub search response parse error: {e}")),
                    });
                }
            },
            false => match serde_json::from_str::<SkillListResponse>(&body) {
                Ok(r) => format_skill_list(&r.items),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("SkillHub list response parse error: {e}")),
                    });
                }
            },
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Format list/search results into a single human-readable string for the LLM.
fn format_skill_list(items: &[SkillListItem]) -> String {
    if items.is_empty() {
        return "SkillHub returned 0 skills.".to_string();
    }
    let mut out = format!("Found {} skill(s) on SkillHub:\n", items.len());
    for (i, item) in items.iter().enumerate() {
        out.push_str(&format_skill_list_item(i + 1, item));
    }
    out
}

fn format_skill_search_results(items: &[SkillSearchItem]) -> String {
    if items.is_empty() {
        return "SkillHub search returned 0 results.".to_string();
    }
    let mut out = format!("Found {} matching skill(s):\n", items.len());
    for (i, item) in items.iter().enumerate() {
        let name = item.display_name.as_deref().unwrap_or(&item.slug);
        let summary = item.summary.as_deref().unwrap_or("(no description)");
        let version = item.version.as_deref().unwrap_or("?");
        out.push_str(&format!(
            "[{i}] {slug} ({name}) v{version} — {summary}",
            i = i + 1,
            slug = item.slug,
            name = name,
            version = version,
            summary = summary,
        ));
        if let Some(s) = item.score {
            out.push_str(&format!(" [score={s:.2}]"));
        }
        out.push('\n');
    }
    out
}

fn format_skill_list_item(idx: usize, item: &SkillListItem) -> String {
    let name = item.display_name.as_deref().unwrap_or(&item.slug);
    let summary = item.summary.as_deref().unwrap_or("(no description)");
    let version = item
        .latest_version
        .as_ref()
        .and_then(|v| v.version.as_deref())
        .unwrap_or("?");
    let downloads = item.stats.as_ref().map(|s| s.downloads).unwrap_or(0);
    format!(
        "[{i}] {slug} ({name}) v{version} — {summary} [↓{downloads}]\n",
        i = idx,
        slug = item.slug,
        name = name,
        version = version,
        summary = summary,
    )
}

impl SkillSearchTool {
    fn list_local_skills(&self) -> anyhow::Result<ToolResult> {
        let skills_path = crate::skills::skills_dir(&self.workspace_dir);
        let installed = crate::skills::load_skills_from_directory(&skills_path, true);
        if installed.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No locally installed skills.".to_string(),
                error: None,
            });
        }
        let mut out = format!("Found {} installed skill(s):\n", installed.len());
        for (i, skill) in installed.iter().enumerate() {
            let tools_count = skill.tools.len();
            let tools_note = if tools_count > 0 {
                format!(" [{} tool(s)]", tools_count)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "[{}] {} v{} — {}{}\n",
                i + 1,
                skill.name,
                skill.version,
                skill.description,
                tools_note,
            ));
        }
        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}

impl Attributable for SkillSearchTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Shell)
    }
    fn alias(&self) -> &str {
        "skill_search"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = r#"{
        "items": [
            {
                "slug": "attendance-query-lite",
                "displayName": "attendance-query-lite",
                "summary": "attendance tool",
                "stats": { "downloads": 21, "stars": 0 },
                "latestVersion": { "version": "20260528.071446" }
            }
        ],
        "nextCursor": null
    }"#;

    const SEARCH_FIXTURE: &str = r#"{
        "results": [
            { "slug": "attendance-query-lite", "displayName": "attendance-query-lite",
              "summary": "attendance tool", "version": "20260528.071446", "score": 0.85 }
        ]
    }"#;

    #[test]
    fn format_list_includes_slug_version_downloads() {
        let resp: SkillListResponse = serde_json::from_str(LIST_FIXTURE).unwrap();
        let s = format_skill_list(&resp.items);
        assert!(s.contains("attendance-query-lite"));
        assert!(s.contains("20260528.071446"));
        assert!(s.contains("21"));
        assert!(s.contains("Found 1 skill"));
    }

    #[test]
    fn format_search_includes_score() {
        let resp: SkillSearchResponse = serde_json::from_str(SEARCH_FIXTURE).unwrap();
        let s = format_skill_search_results(&resp.results);
        assert!(s.contains("attendance-query-lite"));
        assert!(s.contains("score=0.85"));
    }

    #[test]
    fn format_handles_empty_results() {
        assert!(format_skill_list(&[]).contains("0 skills"));
        assert!(format_skill_search_results(&[]).contains("0 results"));
    }
}
