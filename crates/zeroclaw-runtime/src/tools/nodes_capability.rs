use crate::dt_nodes_registry::node_registry::{
    NodeCommandResult, NodeDescription, NodeInfo, NodeRegistry,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_config::schema::GatewayCapabilityControlConfig;
use zeroclaw_log::record;

pub fn wrap_node_registry(
    inner: Arc<dyn NodeRegistry>,
    workspace_dir: &Path,
    capability_control: &GatewayCapabilityControlConfig,
) -> Arc<dyn NodeRegistry> {
    CapabilityFilteringNodeRegistry::wrap(
        inner,
        CapabilityControl::from_workspace_dir(workspace_dir, capability_control, true),
    )
}

/// Filters a list of nodes based on capability control configuration.
/// Returns nodes with their capabilities filtered to only those allowed by the config.
/// This is useful for displaying nodes in system prompts or UI where you want to show
/// only the capabilities that are actually available for use.
pub fn filter_visible_nodes(
    nodes: Vec<NodeInfo>,
    workspace_dir: &Path,
    capability_control: &GatewayCapabilityControlConfig,
) -> Vec<NodeInfo> {
    let Some(control) =
        CapabilityControl::from_workspace_dir(workspace_dir, capability_control, false)
    else {
        return nodes;
    };

    nodes
        .into_iter()
        .map(|mut node| {
            let allowed_capabilities = allowed_capabilities_for_control(
                &control,
                &node.node_id,
                node.meta.as_ref(),
                &node.capabilities,
            );

            node.capabilities = allowed_capabilities.clone();
            if let Some(meta) = node.meta.as_mut() {
                filter_meta_capabilities(meta, &allowed_capabilities);
            }
            node
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CapabilityAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
struct CapabilityRule {
    #[serde(default)]
    nodes: Vec<String>,
    // Option<Vec<String>> 用于区分"字段缺失"和"空数组"
    // None = 节点层级控制（不检查命令）
    // Some([]) = 显式空数组（匹配所有命令）
    // Some([...]) = 具体命令列表
    #[serde(default)]
    commands: Option<Vec<String>>,
    action: CapabilityAction,
}

#[derive(Debug, Clone, Deserialize)]
struct CapabilityControlConfig {
    #[serde(default = "capability_default_true")]
    enabled: bool,
    #[serde(default)]
    rules: Vec<CapabilityRule>,
    #[serde(default)]
    default_allow: bool,
}

#[derive(Debug, Clone)]
struct CapabilityControl {
    rules: Vec<CapabilityRule>,
    default_allow: bool,
}

#[derive(Debug, Clone)]
struct CapabilityNodeAttrs {
    display_name: String,
    instance_id: String,
    device_id: String,
    model_identifier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RulePriority {
    ExactCommand = 1,
    CommandCategory = 2,
    NodeOnly = 3,
    Global = 4,
}

struct CapabilityFilteringNodeRegistry {
    inner: Arc<dyn NodeRegistry>,
    capability_control: CapabilityControl,
}

impl CapabilityNodeAttrs {
    fn from_meta(node_id: &str, meta: Option<&Value>) -> Self {
        let display_name = nested_str(meta, &["client", "displayName"])
            .or_else(|| nested_str(meta, &["displayName"]))
            .unwrap_or(node_id);
        let instance_id = nested_str(meta, &["client", "id"])
            .or_else(|| nested_str(meta, &["instanceId"]))
            .unwrap_or(node_id);
        let device_id = nested_str(meta, &["device", "id"])
            .or_else(|| nested_str(meta, &["deviceId"]))
            .unwrap_or(node_id);
        let model_identifier = nested_str(meta, &["client", "modelIdentifier"])
            .or_else(|| nested_str(meta, &["modelIdentifier"]))
            .unwrap_or(node_id);

        Self {
            display_name: display_name.to_string(),
            instance_id: instance_id.to_string(),
            device_id: device_id.to_string(),
            model_identifier: model_identifier.to_string(),
        }
    }
}

impl CapabilityControl {
    fn from_workspace_dir(
        workspace_dir: &Path,
        capability_control: &GatewayCapabilityControlConfig,
        emit_info_logs: bool,
    ) -> Option<Self> {
        if workspace_dir.as_os_str().is_empty() {
            if emit_info_logs {
                record!(
                    INFO,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                    "nodes tool capability control disabled because workspace_dir is empty; all node capabilities remain enabled"
                );
            }
            return None;
        }

        let config_path = resolve_config_path(workspace_dir, capability_control, emit_info_logs)?;
        let content = match std::fs::read_to_string(&config_path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if emit_info_logs {
                    record!(
                        INFO,
                        zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                            .with_attrs(
                                serde_json::json!({"path": config_path.display().to_string()})
                            ),
                        "nodes tool capability control config not found; capability filtering disabled and all node capabilities remain enabled"
                    );
                }
                return None;
            }
            Err(error) => {
                record!(
                    WARN,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_outcome(zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(serde_json::json!({
                            "path": config_path.display().to_string(),
                            "error": error.to_string()
                        })),
                    "failed to read nodes tool capability control config; capability filtering disabled and all node capabilities remain enabled"
                );
                return None;
            }
        };

        let parsed = match toml::from_str::<CapabilityControlConfig>(&content) {
            Ok(parsed) => parsed,
            Err(error) => {
                record!(
                    WARN,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_outcome(zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(serde_json::json!({
                            "path": config_path.display().to_string(),
                            "error": error.to_string()
                        })),
                    "failed to parse nodes tool capability control config; capability filtering disabled and all node capabilities remain enabled"
                );
                return None;
            }
        };

        if !parsed.enabled {
            if emit_info_logs {
                record!(
                    INFO,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({"path": config_path.display().to_string()})),
                    "nodes tool capability control config loaded but disabled; all node capabilities remain enabled"
                );
            }
            return None;
        }

        if emit_info_logs {
            record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({
                        "path": config_path.display().to_string(),
                        "rules": parsed.rules.len(),
                        "default_allow": parsed.default_allow
                    })
                ),
                "nodes tool capability control config loaded"
            );
        }

        Some(Self {
            rules: parsed.rules,
            default_allow: parsed.default_allow,
        })
    }

    fn evaluate(&self, attrs: &CapabilityNodeAttrs, command: &str) -> CapabilityAction {
        let mut matched: Vec<(&CapabilityRule, RulePriority)> = self
            .rules
            .iter()
            .filter_map(|rule| {
                match_rule_priority(rule, attrs, command).map(|priority| (rule, priority))
            })
            .collect();

        if matched.is_empty() {
            return if self.default_allow {
                CapabilityAction::Allow
            } else {
                CapabilityAction::Deny
            };
        }

        matched.sort_by_key(|(_, priority)| *priority as u8);
        let highest_priority = matched[0].1;

        for (rule, priority) in matched {
            if priority != highest_priority {
                break;
            }
            if rule.action == CapabilityAction::Deny {
                return CapabilityAction::Deny;
            }
        }

        CapabilityAction::Allow
    }

    fn allowed_capabilities<I, S>(
        &self,
        attrs: &CapabilityNodeAttrs,
        capabilities: I,
    ) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        normalize_capability_names(capabilities)
            .into_iter()
            .filter(|capability| self.evaluate(attrs, capability) == CapabilityAction::Allow)
            .collect()
    }
}

impl CapabilityFilteringNodeRegistry {
    fn wrap(
        inner: Arc<dyn NodeRegistry>,
        capability_control: Option<CapabilityControl>,
    ) -> Arc<dyn NodeRegistry> {
        match capability_control {
            Some(capability_control) => Arc::new(Self {
                inner,
                capability_control,
            }),
            None => inner,
        }
    }

    fn filter_node_info(&self, mut node: NodeInfo) -> NodeInfo {
        let allowed_capabilities =
            self.allowed_capabilities_for(&node.node_id, node.meta.as_ref(), &node.capabilities);

        let original_capabilities =
            self.source_capabilities_for(node.meta.as_ref(), &node.capabilities);
        node.capabilities = allowed_capabilities.clone();
        if let Some(meta) = node.meta.as_mut() {
            filter_meta_capabilities(meta, &allowed_capabilities);
            annotate_capability_control(meta, &allowed_capabilities, &original_capabilities);
        }
        node
    }

    fn filter_node_description(&self, mut node: NodeDescription) -> NodeDescription {
        let allowed_capabilities =
            self.allowed_capabilities_for(&node.node_id, node.meta.as_ref(), &node.capabilities);

        let original_capabilities =
            self.source_capabilities_for(node.meta.as_ref(), &node.capabilities);
        node.capabilities = allowed_capabilities.clone();
        if let Some(meta) = node.meta.as_mut() {
            filter_meta_capabilities(meta, &allowed_capabilities);
            annotate_capability_control(meta, &allowed_capabilities, &original_capabilities);
        }
        node
    }

    fn allowed_capabilities_for(
        &self,
        node_id: &str,
        meta: Option<&Value>,
        fallback_capabilities: &[String],
    ) -> Vec<String> {
        // Older sessions may only expose the already-registered capability list, so fall back
        // to that when the original node-advertised capabilities are not present in metadata.
        allowed_capabilities_for_control(
            &self.capability_control,
            node_id,
            meta,
            fallback_capabilities,
        )
    }

    fn source_capabilities_for(
        &self,
        meta: Option<&Value>,
        fallback_capabilities: &[String],
    ) -> Vec<String> {
        source_capabilities_from_meta(meta, fallback_capabilities)
    }

    fn disabled_capability_result(
        &self,
        node_id: &str,
        capability: &str,
    ) -> Option<NodeCommandResult> {
        let node = self.inner.describe(node_id)?;
        let attrs = CapabilityNodeAttrs::from_meta(node_id, node.meta.as_ref());
        // Older sessions may not mirror the original capability list into metadata yet.
        let raw_capabilities = raw_capabilities_from_meta(node.meta.as_ref());
        let source_capabilities = if raw_capabilities.is_empty() {
            normalize_capability_names(node.capabilities.iter().map(String::as_str))
        } else {
            raw_capabilities
        };
        if !source_capabilities.iter().any(|name| name == capability) {
            return None;
        }
        if self.capability_control.evaluate(&attrs, capability) == CapabilityAction::Allow {
            return None;
        }

        Some(NodeCommandResult {
            success: false,
            output: format!(
                "ERROR: Capability '{capability}' is disabled by capability control policy. Check node status/describe to see the currently allowed capabilities."
            ),
            error: Some(format!("Capability '{capability}' is disabled")),
        })
    }
}

#[async_trait]
impl NodeRegistry for CapabilityFilteringNodeRegistry {
    fn list(&self) -> Vec<NodeInfo> {
        self.inner
            .list()
            .into_iter()
            .map(|node| self.filter_node_info(node))
            .collect()
    }

    fn describe(&self, node_id: &str) -> Option<NodeDescription> {
        self.inner
            .describe(node_id)
            .map(|node| self.filter_node_description(node))
    }

    async fn invoke(
        &self,
        node_id: &str,
        capability: &str,
        arguments: Value,
    ) -> anyhow::Result<NodeCommandResult> {
        if let Some(result) = self.disabled_capability_result(node_id, capability) {
            return Ok(result);
        }
        self.inner.invoke(node_id, capability, arguments).await
    }

    async fn run(&self, node_id: &str, raw_command: &str) -> anyhow::Result<NodeCommandResult> {
        if let Some(result) = self.disabled_capability_result(node_id, raw_command) {
            return Ok(result);
        }
        self.inner.run(node_id, raw_command).await
    }
}

fn capability_default_true() -> bool {
    true
}

fn resolve_config_path(
    workspace_dir: &Path,
    capability_control: &GatewayCapabilityControlConfig,
    emit_info_logs: bool,
) -> Option<PathBuf> {
    if !capability_control.enabled {
        if emit_info_logs {
            record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                "nodes tool capability control disabled by gateway.capability_control.enabled=false; all node capabilities remain enabled"
            );
        }
        return None;
    }

    let Some(path) = capability_control
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        if emit_info_logs {
            record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                "nodes tool capability control enabled but gateway.capability_control.path is empty; capability filtering disabled and all node capabilities remain enabled"
            );
        }
        return None;
    };

    let resolved = resolve_path_from_workspace(workspace_dir, path);
    if emit_info_logs {
        record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "configured_path": path,
                    "path": resolved.display().to_string()
                })
            ),
            "nodes tool capability control will use configured path"
        );
    }
    Some(resolved)
}

fn resolve_path_from_workspace(workspace_dir: &Path, configured_path: &str) -> PathBuf {
    let expanded = shellexpand::tilde(configured_path).into_owned();
    let path = PathBuf::from(expanded);
    if path.is_relative() {
        workspace_dir.join(path)
    } else {
        path
    }
}

fn allowed_capabilities_for_control(
    capability_control: &CapabilityControl,
    node_id: &str,
    meta: Option<&Value>,
    fallback_capabilities: &[String],
) -> Vec<String> {
    let attrs = CapabilityNodeAttrs::from_meta(node_id, meta);
    let raw_capabilities = raw_capabilities_from_meta(meta);
    let source_capabilities = if raw_capabilities.is_empty() {
        normalize_capability_names(fallback_capabilities.iter().map(String::as_str))
    } else {
        raw_capabilities
    };
    capability_control.allowed_capabilities(&attrs, source_capabilities.iter().map(String::as_str))
}

fn nested_str<'a>(value: Option<&'a Value>, path: &[&str]) -> Option<&'a str> {
    let mut current = value?;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn normalize_capability_names<I, S>(capabilities: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut normalized = Vec::new();

    for capability in capabilities {
        let trimmed = capability.as_ref().trim();
        if trimmed.is_empty() || normalized.iter().any(|existing| existing == trimmed) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }

    normalized
}

fn raw_capabilities_from_meta(meta: Option<&Value>) -> Vec<String> {
    let Some(meta) = meta else {
        return Vec::new();
    };

    let mut capabilities = Vec::new();
    for key in ["caps", "commands", "capabilities"] {
        let Some(values) = meta.get(key).and_then(Value::as_array) else {
            continue;
        };
        capabilities.extend(values.iter().filter_map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }));
    }

    normalize_capability_names(capabilities.iter().map(String::as_str))
}

fn source_capabilities_from_meta(
    meta: Option<&Value>,
    fallback_capabilities: &[String],
) -> Vec<String> {
    let raw_capabilities = raw_capabilities_from_meta(meta);
    if raw_capabilities.is_empty() {
        normalize_capability_names(fallback_capabilities.iter().map(String::as_str))
    } else {
        raw_capabilities
    }
}

fn filter_meta_capabilities(meta: &mut Value, allowed_capabilities: &[String]) {
    let Some(obj) = meta.as_object_mut() else {
        return;
    };

    for key in ["caps", "commands", "capabilities"] {
        let Some(values) = obj.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        values.retain(|value| {
            value
                .as_str()
                .map(str::trim)
                .is_some_and(|name| allowed_capabilities.iter().any(|allowed| allowed == name))
        });
    }
}

fn annotate_capability_control(
    meta: &mut Value,
    allowed_capabilities: &[String],
    original_capabilities: &[String],
) {
    let Some(obj) = meta.as_object_mut() else {
        return;
    };

    if allowed_capabilities.len() == original_capabilities.len() {
        obj.remove("capabilityControl");
        return;
    }

    let disabled_count = original_capabilities
        .len()
        .saturating_sub(allowed_capabilities.len());
    obj.insert(
        "capabilityControl".to_string(),
        serde_json::json!({
            "status": "filtered",
            "allowedCount": allowed_capabilities.len(),
            "disabledCount": disabled_count,
            "message": "Capabilities filtered by capability control policy"
        }),
    );
}

fn calculate_rule_priority(rule: &CapabilityRule) -> RulePriority {
    let has_nodes = !rule.nodes.is_empty();

    let Some(commands) = rule.commands.as_ref() else {
        return if has_nodes {
            RulePriority::NodeOnly
        } else {
            RulePriority::Global
        };
    };

    if commands.is_empty() {
        return RulePriority::CommandCategory;
    }

    if commands
        .iter()
        .any(|command| is_exact_command_pattern(command))
    {
        return RulePriority::ExactCommand;
    }

    if commands
        .iter()
        .any(|command| is_command_category_pattern(command))
    {
        return RulePriority::CommandCategory;
    }

    if has_nodes {
        RulePriority::NodeOnly
    } else {
        RulePriority::Global
    }
}

fn match_rule_priority(
    rule: &CapabilityRule,
    attrs: &CapabilityNodeAttrs,
    command: &str,
) -> Option<RulePriority> {
    let node_match = rule.nodes.is_empty()
        || rule.nodes.contains(&attrs.display_name)
        || rule.nodes.contains(&attrs.instance_id)
        || rule.nodes.contains(&attrs.device_id)
        || rule.nodes.contains(&attrs.model_identifier);

    if !node_match {
        return None;
    }

    match rule.commands.as_ref() {
        None => Some(calculate_rule_priority(rule)),
        Some(commands) if commands.is_empty() => Some(RulePriority::CommandCategory),
        Some(commands) => commands
            .iter()
            .filter_map(|pattern| match_capability_pattern_priority(pattern, command))
            .min(),
    }
}

fn is_exact_command_pattern(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty() && !trimmed.contains('*')
}

fn is_command_category_pattern(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    trimmed == "*" || trimmed.contains('*')
}

fn match_capability_pattern_priority(pattern: &str, command: &str) -> Option<RulePriority> {
    if !match_capability_pattern(pattern, command) {
        return None;
    }

    if is_exact_command_pattern(pattern) {
        Some(RulePriority::ExactCommand)
    } else {
        Some(RulePriority::CommandCategory)
    }
}

fn match_capability_pattern(pattern: &str, command: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix(".*") {
        return command == prefix || command.starts_with(&format!("{prefix}."));
    }

    let pattern_parts: Vec<&str> = pattern.split('.').collect();
    let command_parts: Vec<&str> = command.split('.').collect();
    if pattern_parts.len() != command_parts.len() {
        return false;
    }

    pattern_parts
        .iter()
        .zip(command_parts.iter())
        .all(|(pattern_part, command_part)| pattern_part == &"*" || pattern_part == command_part)
}

#[cfg(test)]
mod tests {
    use super::super::{NodesTool, Tool};
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_config::schema::GatewayCapabilityControlConfig;

    const TEST_CAPABILITY_CONTROL_FILE: &str = "capability_control.toml";

    struct MockNodeRegistry {
        nodes: Vec<NodeInfo>,
        described: Option<NodeDescription>,
        invoke_count: AtomicUsize,
    }

    #[async_trait]
    impl NodeRegistry for MockNodeRegistry {
        fn list(&self) -> Vec<NodeInfo> {
            self.nodes.clone()
        }

        fn describe(&self, _node_id: &str) -> Option<NodeDescription> {
            self.described.clone()
        }

        async fn invoke(
            &self,
            _node_id: &str,
            _capability: &str,
            _arguments: Value,
        ) -> anyhow::Result<NodeCommandResult> {
            self.invoke_count.fetch_add(1, Ordering::SeqCst);
            Ok(NodeCommandResult {
                success: true,
                output: serde_json::json!({ "ok": true }).to_string(),
                error: None,
            })
        }

        async fn run(
            &self,
            _node_id: &str,
            _raw_command: &str,
        ) -> anyhow::Result<NodeCommandResult> {
            self.invoke_count.fetch_add(1, Ordering::SeqCst);
            Ok(NodeCommandResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    fn write_capability_config(dir: &Path, body: &str) -> PathBuf {
        let workspace_dir = dir.join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        let capability_path = workspace_dir.join(TEST_CAPABILITY_CONTROL_FILE);
        std::fs::write(&capability_path, body).expect("write capability control config");
        workspace_dir
    }

    fn raw_node_meta() -> Value {
        serde_json::json!({
            "client": {
                "displayName": "Redmi 10X 4G",
                "id": "openclaw-android"
            },
            "device": {
                "id": "8af674d7"
            },
            "caps": ["flashlight.turnOn", "camera.snap"],
            "commands": ["camera.snap"]
        })
    }

    #[tokio::test]
    async fn status_filters_disabled_capabilities_from_visible_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_dir = write_capability_config(
            temp.path(),
            r#"
enabled = true
default_allow = false

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["flashlight.*"]
action = "allow"

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["camera.*"]
action = "deny"
"#,
        );

        let registry = Arc::new(MockNodeRegistry {
            nodes: vec![NodeInfo {
                node_id: "redmi".to_string(),
                status: "connected".to_string(),
                capabilities: vec!["flashlight.turnOn".to_string(), "camera.snap".to_string()],
                meta: Some(raw_node_meta()),
                events: None,
            }],
            described: None,
            invoke_count: AtomicUsize::new(0),
        });

        let tool = NodesTool::new_with_capability_control(
            registry,
            &workspace_dir,
            GatewayCapabilityControlConfig {
                enabled: true,
                path: Some(TEST_CAPABILITY_CONTROL_FILE.to_string()),
            },
        );
        let result = tool
            .execute(serde_json::json!({ "action": "status" }))
            .await
            .expect("status should succeed");

        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.output).expect("status output json");
        let node = &payload["nodes"][0];
        assert_eq!(
            node["capabilities"],
            serde_json::json!(["flashlight.turnOn"])
        );
        assert_eq!(
            node["meta"]["caps"],
            serde_json::json!(["flashlight.turnOn"])
        );
        assert_eq!(node["meta"]["commands"], serde_json::json!([]));
        assert_eq!(
            node["meta"]["capabilityControl"]["status"],
            serde_json::json!("filtered")
        );
        assert_eq!(
            node["meta"]["capabilityControl"]["disabledCount"],
            serde_json::json!(1)
        );
    }

    #[tokio::test]
    async fn invoke_returns_disabled_message_without_calling_node() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_dir = write_capability_config(
            temp.path(),
            r#"
enabled = true
default_allow = false

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["flashlight.*"]
action = "allow"

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["camera.*"]
action = "deny"
"#,
        );

        let described = NodeDescription {
            node_id: "redmi".to_string(),
            status: "connected".to_string(),
            capabilities: vec!["flashlight.turnOn".to_string(), "camera.snap".to_string()],
            meta: Some(raw_node_meta()),
            events: None,
        };
        let registry = Arc::new(MockNodeRegistry {
            nodes: vec![NodeInfo {
                node_id: "redmi".to_string(),
                status: "connected".to_string(),
                capabilities: vec!["flashlight.turnOn".to_string(), "camera.snap".to_string()],
                meta: Some(raw_node_meta()),
                events: None,
            }],
            described: Some(described),
            invoke_count: AtomicUsize::new(0),
        });

        let tool = NodesTool::new_with_capability_control(
            registry.clone(),
            &workspace_dir,
            GatewayCapabilityControlConfig {
                enabled: true,
                path: Some(TEST_CAPABILITY_CONTROL_FILE.to_string()),
            },
        );
        let result = tool
            .execute(serde_json::json!({
                "action": "invoke",
                "node": "redmi",
                "invokeCommand": "camera.snap",
                "invokeParamsJson": "{}"
            }))
            .await
            .expect("invoke should return a tool result");

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("Capability 'camera.snap' is disabled")
        );
        assert_eq!(registry.invoke_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn configured_capability_control_path_is_used() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_dir = temp.path().join("workspace");
        let config_dir = workspace_dir.join("capability");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join(TEST_CAPABILITY_CONTROL_FILE),
            r#"
enabled = true
default_allow = false

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["flashlight.*"]
action = "allow"
"#,
        )
        .expect("write capability control config");

        let registry = Arc::new(MockNodeRegistry {
            nodes: vec![NodeInfo {
                node_id: "redmi".to_string(),
                status: "connected".to_string(),
                capabilities: vec!["flashlight.turnOn".to_string(), "camera.snap".to_string()],
                meta: Some(raw_node_meta()),
                events: None,
            }],
            described: None,
            invoke_count: AtomicUsize::new(0),
        });

        let tool = NodesTool::new_with_capability_control(
            registry,
            &workspace_dir,
            GatewayCapabilityControlConfig {
                enabled: true,
                path: Some(format!("capability/{TEST_CAPABILITY_CONTROL_FILE}")),
            },
        );
        let result = tool
            .execute(serde_json::json!({ "action": "status" }))
            .await
            .expect("status should succeed");

        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.output).expect("status output json");
        let node = &payload["nodes"][0];
        assert_eq!(
            node["capabilities"],
            serde_json::json!(["flashlight.turnOn"])
        );
    }

    #[tokio::test]
    async fn capability_control_root_can_differ_from_workspace_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_dir = temp.path().join("workspace");
        let control_root = temp.path().join("data");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        std::fs::create_dir_all(&control_root).expect("create control root");
        std::fs::write(
            control_root.join(TEST_CAPABILITY_CONTROL_FILE),
            r#"
enabled = true
default_allow = false

[[rules]]
nodes = ["Redmi 10X 4G"]
commands = ["flashlight.*"]
action = "allow"
"#,
        )
        .expect("write capability control config");

        let registry = Arc::new(MockNodeRegistry {
            nodes: vec![NodeInfo {
                node_id: "redmi".to_string(),
                status: "connected".to_string(),
                capabilities: vec!["flashlight.turnOn".to_string(), "camera.snap".to_string()],
                meta: Some(raw_node_meta()),
                events: None,
            }],
            described: None,
            invoke_count: AtomicUsize::new(0),
        });

        let tool = NodesTool::new_with_capability_control_root(
            registry,
            &workspace_dir,
            &control_root,
            GatewayCapabilityControlConfig {
                enabled: true,
                path: Some(TEST_CAPABILITY_CONTROL_FILE.to_string()),
            },
        );
        let result = tool
            .execute(serde_json::json!({ "action": "status" }))
            .await
            .expect("status should succeed");

        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.output).expect("status output json");
        let node = &payload["nodes"][0];
        assert_eq!(
            node["capabilities"],
            serde_json::json!(["flashlight.turnOn"])
        );
    }

    #[test]
    fn wildcard_command_rule_outranks_node_only_rule() {
        let attrs = CapabilityNodeAttrs::from_meta("redmi", Some(&raw_node_meta()));
        let control = CapabilityControl {
            rules: vec![
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: None,
                    action: CapabilityAction::Deny,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: Some(vec!["*".to_string()]),
                    action: CapabilityAction::Allow,
                },
            ],
            default_allow: false,
        };

        assert_eq!(
            control.evaluate(&attrs, "camera.snap"),
            CapabilityAction::Allow
        );
    }

    #[test]
    fn exact_category_node_and_global_priorities_follow_requested_order() {
        let attrs = CapabilityNodeAttrs::from_meta("redmi", Some(&raw_node_meta()));
        let control = CapabilityControl {
            rules: vec![
                CapabilityRule {
                    nodes: vec![],
                    commands: None,
                    action: CapabilityAction::Allow,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: None,
                    action: CapabilityAction::Deny,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: Some(vec!["photos.*".to_string()]),
                    action: CapabilityAction::Allow,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: Some(vec!["photos.delete".to_string()]),
                    action: CapabilityAction::Deny,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: Some(vec!["camera.snap".to_string()]),
                    action: CapabilityAction::Allow,
                },
                CapabilityRule {
                    nodes: vec!["Redmi 10X 4G".to_string()],
                    commands: Some(vec!["camera.snap".to_string()]),
                    action: CapabilityAction::Deny,
                },
            ],
            default_allow: false,
        };

        assert_eq!(
            control.evaluate(&attrs, "photos.delete"),
            CapabilityAction::Deny
        );
        assert_eq!(
            control.evaluate(&attrs, "photos.latest"),
            CapabilityAction::Allow
        );
        assert_eq!(
            control.evaluate(&attrs, "notifications.list"),
            CapabilityAction::Deny
        );
        assert_eq!(
            control.evaluate(&attrs, "camera.snap"),
            CapabilityAction::Deny
        );
    }
}
