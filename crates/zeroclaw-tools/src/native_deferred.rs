use std::collections::HashSet;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolSpec};

#[derive(Clone)]
pub struct DeferredNativeToolStub {
    pub name: String,
    pub description: String,
    tool: Arc<dyn Tool>,
}

impl DeferredNativeToolStub {
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            tool,
        }
    }

    pub fn tool_arc(&self) -> Arc<dyn Tool> {
        Arc::clone(&self.tool)
    }

    pub fn spec(&self) -> ToolSpec {
        self.tool.spec()
    }
}

#[derive(Clone, Default)]
pub struct DeferredNativeToolSet {
    pub stubs: Vec<DeferredNativeToolStub>,
}

impl DeferredNativeToolSet {
    pub fn empty() -> Self {
        Self { stubs: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.stubs.is_empty()
    }

    pub fn search(&self, query: &str, max_results: usize) -> Vec<&DeferredNativeToolStub> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.stubs
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q) || s.description.to_lowercase().contains(&q)
            })
            .take(max_results)
            .collect()
    }

    pub fn tool_spec(&self, name: &str) -> Option<ToolSpec> {
        self.stubs
            .iter()
            .find(|s| s.name == name)
            .map(DeferredNativeToolStub::spec)
    }

    pub fn activate_arc(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.stubs
            .iter()
            .find(|s| s.name == name)
            .map(DeferredNativeToolStub::tool_arc)
    }
}

pub fn default_active_native_tool_names() -> HashSet<String> {
    ["file_read", "read_skill"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub fn build_active_native_tool_set(config_keep: &[String]) -> HashSet<String> {
    let mut s = default_active_native_tool_names();
    for n in config_keep {
        let t = n.trim();
        if !t.is_empty() {
            s.insert(t.to_string());
        }
    }
    s
}

pub fn partition_tools_for_native_deferred(
    tools: Vec<Box<dyn Tool>>,
    keep: &HashSet<String>,
) -> (Vec<Box<dyn Tool>>, DeferredNativeToolSet) {
    let mut kept = Vec::with_capacity(tools.len());
    let mut stubs = Vec::new();
    for tool in tools {
        let name = tool.name().to_string();
        if keep.contains(&name) {
            kept.push(tool);
            continue;
        }
        let arc: Arc<dyn Tool> = Arc::from(tool);
        stubs.push(DeferredNativeToolStub::new(arc));
    }
    (kept, DeferredNativeToolSet { stubs })
}
