//! Wires MCP deferred loading, native deferred loading, and a single `tool_search` instance.

use std::sync::{Arc, Mutex};

use zeroclaw_config::schema::Config;

use zeroclaw_tools::native_deferred::{
    build_active_native_tool_set, partition_tools_for_native_deferred, DeferredNativeToolSet,
};
use super::{
    ActivatedToolSet, ArcToolRef, DelegateParentToolsHandle, DeferredMcpToolSet, McpRegistry,
    McpToolWrapper, Tool, ToolSearchTool,
};
use crate::security::AutonomyLevel;

/// Result of [`wire_deferred_tool_surfaces`].
pub struct DeferredWireOutcome {
    /// Combined deferred-tools block for the system prompt (MCP + built-in).
    pub deferred_prompt_section: String,
    /// Set when `tool_search` was registered; same handle the agent loop uses for activation.
    pub activated_tools: Option<Arc<Mutex<ActivatedToolSet>>>,
}

fn build_combined_deferred_section(
    config: &Config,
    mcp: &DeferredMcpToolSet,
    native: &DeferredNativeToolSet,
) -> String {
    if mcp.is_empty() && native.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("## Deferred Tools\n\n");
    out.push_str(
        "The tools listed below are available but NOT yet loaded. \
         To use any of them you MUST first call the `tool_search` tool \
         to fetch their full schemas. Use `\"select:name1,name2\"` for \
         exact tools or keywords to search. Once activated, the tools \
         become callable for the rest of the conversation.\n\n",
    );
    out.push_str("<available-deferred-tools>\n");
    for stub in &mcp.stubs {
        out.push_str(&stub.prefixed_name);
        out.push_str(" - ");
        out.push_str(&stub.description);
        out.push('\n');
    }
    let mut tool_descs = native.stubs.clone();
    let excluded = &config.autonomy.non_cli_excluded_tools;
    if !excluded.is_empty() && config.autonomy.level != AutonomyLevel::Full {
        tool_descs.retain(|stub| !excluded.iter().any(|ex| *ex == stub.name));
    }
    for stub in &tool_descs {
        out.push_str("- ");
        out.push_str(&stub.name);
        out.push_str(":");
        out.push_str(&stub.description);
        out.push('\n');
    }
    out.push_str("</available-deferred-tools>\n");
    out
}

/// Connect MCP (eager or deferred), optionally partition built-in tools for deferred loading,
/// and register exactly one [`ToolSearchTool`] when needed.
pub async fn wire_deferred_tool_surfaces(
    config: &Config,
    tools: &mut Vec<Box<dyn Tool>>,
    delegate_parent: Option<&DelegateParentToolsHandle>,
) -> DeferredWireOutcome {
    let mut mcp_set = DeferredMcpToolSet::empty().await;
    let mut mcp_deferred_active = false;

    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = Arc::new(registry);
                if config.mcp.deferred_loading {
                    mcp_set = DeferredMcpToolSet::from_registry(Arc::clone(&registry)).await;
                    mcp_deferred_active = true;
                    tracing::info!(
                        "MCP deferred: {} tool stub(s) from {} server(s)",
                        mcp_set.len(),
                        registry.server_count()
                    );
                } else {
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: Arc<dyn Tool> = Arc::new(McpToolWrapper::new(
                                name,
                                def,
                                Arc::clone(&registry),
                            ));
                            if let Some(handle) = delegate_parent {
                                handle.write().push(Arc::clone(&wrapper));
                            }
                            tools.push(Box::new(ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    tracing::info!(
                        "MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    let mut native_set = DeferredNativeToolSet::empty();
    if config.agent.native_deferred_loading_enabled {
        let keep = build_active_native_tool_set(&config.agent.native_active_tools);
        let taken = std::mem::take(tools);
        let (kept, deferred) = partition_tools_for_native_deferred(taken, &keep);
        *tools = kept;
        if !deferred.is_empty() {
            tracing::info!(
                count = deferred.stubs.len(),
                "Native deferred: built-in tool(s) moved to deferred catalog"
            );
            native_set = deferred;
        }
    }

    let push_tool_search = mcp_deferred_active || !native_set.is_empty();
    let deferred_prompt_section = if push_tool_search {
        build_combined_deferred_section(config, &mcp_set, &native_set)
    } else {
        String::new()
    };

    let activated_tools = if push_tool_search {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        tools.push(Box::new(ToolSearchTool::new(
            mcp_set,
            native_set,
            Arc::clone(&activated),
        )));
        Some(activated)
    } else {
        None
    };

    DeferredWireOutcome {
        deferred_prompt_section,
        activated_tools,
    }
}
