use crate::config::Config;
use std::fmt::Write;

pub fn build_channel_system_prompt(
    base_prompt: &str,
    config: &Config,
    channel_name: &str,
    reply_target: &str,
) -> String {
    let connected_nodes_section = build_connected_nodes_section(config);
    let delivery = super::channel_delivery_instructions(channel_name).map(str::to_string);
    zeroclaw_runtime::channel_system_prompt::build_channel_system_prompt_base(
        base_prompt,
        config,
        channel_name,
        reply_target,
        connected_nodes_section.as_deref(),
        delivery.as_deref(),
    )
}

fn build_connected_nodes_section(config: &Config) -> Option<String> {
    if !config.gateway.node_control.enabled {
        return None;
    }
    let mut out = String::new();
    out.push_str("## Connected Nodes/Devices\n\n");
    out.push_str("You can use the nodes tool to control the nodes.\n");
    let nodes = crate::dt_nodes_registry::ConnectedNodeRegistry::global().list();
    if nodes.is_empty() {
        out.push_str("- **No nodes connected.**\n\n");
        return Some(out);
    }
    out.push_str("Currently connected nodes (`nodes status` tool execute result, you don't need to execute it again):\n");
    for node in nodes {
        let display_name = node
            .meta
            .as_ref()
            .and_then(|m| m.get("client"))
            .and_then(|c| c.get("displayName").or_else(|| c.get("display_name")))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let capabilities = if node.capabilities.is_empty() {
            "none".to_string()
        } else {
            node.capabilities.join(", ")
        };
        let _ = writeln!(
            out,
            "- Name: `{}` | ID: `{}` | Status: `{}` | Capabilities: {}",
            display_name,
            node.node_id,
            node.status,
            capabilities
        );
    }
    out.push('\n');
    Some(out)
}
