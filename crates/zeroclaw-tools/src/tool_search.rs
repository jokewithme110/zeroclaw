//! Built-in `tool_search` tool for on-demand MCP tool schema loading.
//!
//! When `mcp.deferred_loading` is enabled, this tool lets the LLM discover and
//! activate deferred MCP tools. Supports two query modes:
//! - `select:name1,name2` — fetch exact tools by prefixed name.
//! - Free-text keyword search — returns the best-matching stubs.

use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::collections::HashSet;
use async_trait::async_trait;

use crate::mcp_deferred::{ActivatedToolSet, DeferredMcpToolSet, DeferredMcpToolStub};
use crate::native_deferred::{DeferredNativeToolSet, DeferredNativeToolStub};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Default maximum number of search results.
const DEFAULT_MAX_RESULTS: usize = 5;

enum DeferredHit<'a> {
    Mcp(&'a DeferredMcpToolStub),
    Native(&'a DeferredNativeToolStub),
}

/// Built-in tool that discovers and activates deferred MCP and/or built-in tools.
pub struct ToolSearchTool {
    mcp: DeferredMcpToolSet,
    native: DeferredNativeToolSet,
    activated: Arc<Mutex<ActivatedToolSet>>,
}

impl ToolSearchTool {
    pub fn new(
        mcp: DeferredMcpToolSet,
        native: DeferredNativeToolSet,
        activated: Arc<Mutex<ActivatedToolSet>>,
    ) -> Self {
        Self {
            mcp,
            native,
            activated,
        }
    }

    fn combined_search(&self, query: &str, max_results: usize) -> Vec<DeferredHit<'_>> {
        let mcp_hits = self.mcp.search(query, max_results);
        let mut out: Vec<DeferredHit<'_>> = mcp_hits.into_iter().map(DeferredHit::Mcp).collect();
        let mut seen: HashSet<String> = out
            .iter()
            .filter_map(|h| match h {
                DeferredHit::Mcp(s) => Some(s.prefixed_name.clone()),
                DeferredHit::Native(s) => Some(s.name.clone()),
            })
            .collect();

        for s in self.native.search(query, max_results) {
            if out.len() >= max_results {
                break;
            }
            if seen.contains(&s.name) {
                continue;
            }
            seen.insert(s.name.clone());
            out.push(DeferredHit::Native(s));
        }
        out
    }

    fn tool_spec_any(&self, name: &str) -> Option<zeroclaw_api::tool::ToolSpec> {
        self.mcp
            .tool_spec(name)
            .or_else(|| self.native.tool_spec(name))
    }

    fn select_tools(&self, names: &[&str]) -> anyhow::Result<ToolResult> {
        let mut output = String::from("<functions>\n");
        let mut not_found = Vec::new();
        let mut activated_count = 0;
        let mut guard = self.activated.lock().unwrap();

        for name in names {
            if name.is_empty() {
                continue;
            }
            if let Some(spec) = self.tool_spec_any(name) {
                if !guard.is_activated(name) {
                    if let Some(tool) = self.mcp.activate(name) {
                        guard.activate(String::from(*name), Arc::from(tool));
                        activated_count += 1;
                    } else if let Some(arc) = self.native.activate_arc(name) {
                        guard.activate(String::from(*name), arc);
                        activated_count += 1;
                    }
                }
                let _ = writeln!(
                    output,
                    "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
                    spec.name,
                    spec.description.replace('"', "\\\""),
                    spec.parameters
                );
            } else {
                not_found.push(*name);
            }
        }

        output.push_str("</functions>\n");
        drop(guard);

        if !not_found.is_empty() {
            let _ = write!(output, "\nNot found: {}", not_found.join(", "));
        }

        tracing::debug!(
            "tool_search select: requested={}, activated={activated_count}, not_found={}",
            names.len(),
            not_found.len()
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Fetch full schema definitions for deferred tools (MCP and/or built-in) so they can be called. \
         Use \"select:name1,name2\" for exact match or keywords to search."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "description": "Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.",
                    "type": "string"
                },
                "max_results": {
                    "description": "Maximum number of results to return (default: 5)",
                    "type": "number",
                    "default": DEFAULT_MAX_RESULTS
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| usize::try_from(v).unwrap_or(DEFAULT_MAX_RESULTS))
            .unwrap_or(DEFAULT_MAX_RESULTS);

        if query.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("query parameter is required".into()),
            });
        }

        if let Some(names_str) = query.strip_prefix("select:") {
            let names: Vec<&str> = names_str.split(',').map(str::trim).collect();
            return self.select_tools(&names);
        }

        let results = self.combined_search(query, max_results);
        if results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No matching deferred tools found.".into(),
                error: None,
            });
        }

        let mut output = String::from("<functions>\n");
        let mut activated_count = 0;
        let mut guard = self.activated.lock().unwrap();

        for hit in &results {
            match hit {
                DeferredHit::Mcp(stub) => {
                    if let Some(spec) = self.mcp.tool_spec(&stub.prefixed_name) {
                        if !guard.is_activated(&stub.prefixed_name) {
                            if let Some(tool) = self.mcp.activate(&stub.prefixed_name) {
                                guard.activate(stub.prefixed_name.clone(), Arc::from(tool));
                                activated_count += 1;
                            }
                        }
                        let _ = writeln!(
                            output,
                            "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
                            spec.name,
                            spec.description.replace('"', "\\\""),
                            spec.parameters
                        );
                    }
                }
                DeferredHit::Native(stub) => {
                    let spec = stub.spec();
                    if !guard.is_activated(&stub.name) {
                        guard.activate(stub.name.clone(), stub.tool_arc());
                        activated_count += 1;
                    }
                    let _ = writeln!(
                        output,
                        "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
                        spec.name,
                        spec.description.replace('"', "\\\""),
                        spec.parameters
                    );
                }
            }
        }

        output.push_str("</functions>\n");
        drop(guard);

        tracing::debug!(
            "tool_search: query={query:?}, matched={}, activated={activated_count}",
            results.len()
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_client::McpRegistry;
    use crate::mcp_deferred::DeferredMcpToolStub;
    use crate::mcp_protocol::McpToolDef;
    use crate::native_deferred::DeferredNativeToolStub;
    use async_trait::async_trait;

    async fn make_mcp_set(stubs: Vec<DeferredMcpToolStub>) -> DeferredMcpToolSet {
        let registry = Arc::new(McpRegistry::connect_all(&[]).await.unwrap());
        DeferredMcpToolSet { stubs, registry }
    }

    fn make_stub(name: &str, desc: &str) -> DeferredMcpToolStub {
        let def = McpToolDef {
            name: name.to_string(),
            description: Some(desc.to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        DeferredMcpToolStub::new(name.to_string(), def)
    }

    struct NamedTool {
        n: &'static str,
        d: &'static str,
    }

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.n
        }
        fn description(&self) -> &str {
            self.d
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"x":{"type":"string"}}})
        }
        async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    fn make_native_set(names: &[(&'static str, &'static str)]) -> DeferredNativeToolSet {
        let stubs: Vec<_> = names
            .iter()
            .map(|&(n, d)| {
                let arc: Arc<dyn Tool> = Arc::new(NamedTool { n, d });
                DeferredNativeToolStub::new(arc)
            })
            .collect();
        DeferredNativeToolSet { stubs }
    }

    #[tokio::test]
    async fn tool_metadata() {
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![]).await,
            DeferredNativeToolSet::empty(),
            Arc::new(Mutex::new(ActivatedToolSet::new())),
        );
        assert_eq!(tool.name(), "tool_search");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn empty_query_returns_error() {
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![]).await,
            DeferredNativeToolSet::empty(),
            Arc::new(Mutex::new(ActivatedToolSet::new())),
        );
        let result = tool
            .execute(serde_json::json!({"query": ""}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn select_nonexistent_tool_reports_not_found() {
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![]).await,
            DeferredNativeToolSet::empty(),
            Arc::new(Mutex::new(ActivatedToolSet::new())),
        );
        let result = tool
            .execute(serde_json::json!({"query": "select:nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Not found"));
    }

    #[tokio::test]
    async fn keyword_search_no_matches() {
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![make_stub("fs__read", "Read file")]).await,
            DeferredNativeToolSet::empty(),
            Arc::new(Mutex::new(ActivatedToolSet::new())),
        );
        let result = tool
            .execute(serde_json::json!({"query": "zzzzz_nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No matching"));
    }

    #[tokio::test]
    async fn keyword_search_finds_match() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![make_stub("fs__read", "Read a file from disk")]).await,
            DeferredNativeToolSet::empty(),
            Arc::clone(&activated),
        );
        let result = tool
            .execute(serde_json::json!({"query": "read file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("<function>"));
        assert!(result.output.contains("fs__read"));
        assert!(activated.lock().unwrap().is_activated("fs__read"));
    }

    #[tokio::test]
    async fn native_deferred_keyword_search() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let native = make_native_set(&[("weather_tool", "Get weather forecast for a city")]);
        let tool = ToolSearchTool::new(make_mcp_set(vec![]).await, native, Arc::clone(&activated));
        let result = tool
            .execute(serde_json::json!({"query": "weather forecast"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("weather_tool"));
        assert!(activated.lock().unwrap().is_activated("weather_tool"));
    }

    #[tokio::test]
    async fn native_select_mode() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let native = make_native_set(&[("pdf_read", "Read PDF files")]);
        let tool = ToolSearchTool::new(make_mcp_set(vec![]).await, native, Arc::clone(&activated));
        let result = tool
            .execute(serde_json::json!({"query": "select:pdf_read"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(activated.lock().unwrap().is_activated("pdf_read"));
    }

    #[tokio::test]
    async fn multiple_servers_stubs_all_searchable() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("server_a__list_files", "List files on server A"),
            make_stub("server_a__read_file", "Read file on server A"),
            make_stub("server_b__query_db", "Query database on server B"),
            make_stub("server_b__insert_row", "Insert row on server B"),
        ];
        let tool = ToolSearchTool::new(
            make_mcp_set(stubs).await,
            DeferredNativeToolSet::empty(),
            Arc::clone(&activated),
        );

        let result = tool
            .execute(serde_json::json!({"query": "file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_a__list_files"));
        assert!(result.output.contains("server_a__read_file"));

        let result = tool
            .execute(serde_json::json!({"query": "database query"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_b__query_db"));
    }

    #[tokio::test]
    async fn select_activates_and_persists_across_calls() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("srv__tool_a", "Tool A"),
            make_stub("srv__tool_b", "Tool B"),
        ];
        let tool = ToolSearchTool::new(
            make_mcp_set(stubs).await,
            DeferredNativeToolSet::empty(),
            Arc::clone(&activated),
        );

        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_a"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(activated.lock().unwrap().is_activated("srv__tool_a"));
        assert!(!activated.lock().unwrap().is_activated("srv__tool_b"));

        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_b"}))
            .await
            .unwrap();
        assert!(result.success);

        let guard = activated.lock().unwrap();
        assert!(guard.is_activated("srv__tool_a"));
        assert!(guard.is_activated("srv__tool_b"));
        assert_eq!(guard.tool_specs().len(), 2);
    }

    #[tokio::test]
    async fn reactivation_is_idempotent() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_mcp_set(vec![make_stub("srv__tool", "A tool")]).await,
            DeferredNativeToolSet::empty(),
            Arc::clone(&activated),
        );

        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();
        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();

        assert_eq!(activated.lock().unwrap().tool_specs().len(), 1);
    }
}
