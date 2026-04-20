use zeroclaw_config::schema::Config;

use std::fmt::Write;
use std::path::Path;
use zeroclaw_runtime::agent::system_prompt::load_openclaw_bootstrap_files;
use zeroclaw_runtime::dt_nodes_registry::{ConnectedNodeRegistry, NodeRegistry};

pub fn build_channel_system_prompt(
    base_prompt: &str,
    config: &Config,
    channel_name: &str,
    reply_target: &str,
) -> String {
    let mut prompt = String::with_capacity(8192);

    prompt.push_str("You are a personal assistant running inside ZeroClaw.  \n\n");

    inject_safety_prompt(&mut prompt);

    inject_tools_prompt(&mut prompt, config, base_prompt);

    inject_skills_prompt(&mut prompt, config);

    inject_workspace_prompt(&mut prompt, config);

    inject_bootstrap_files_prompt(&mut prompt, config);

    inject_a2a_prompt(&mut prompt, config, &config.workspace_dir);

    inject_current_date_time_prompt(&mut prompt);

    inject_runtime_prompt(&mut prompt, config);

    inject_connected_nodes_prompt(&mut prompt, config);

    inject_channel_delivery_instructions_prompt(&mut prompt, channel_name);

    inject_channel_context_prompt(&mut prompt, channel_name, reply_target);

    compact_system_prompt_if_needed(&mut prompt, config.agent.max_system_prompt_chars);

    prompt
}

fn inject_tools_prompt(prompt: &mut String, config: &Config, base_prompt: &str) {
    if config.agent.parallel_tools {
        prompt.push_str("## Tools \n\n");
        prompt.push_str("Support parallel tool calls. When tools are independent with no dependencies, call them simultaneously in one response. Only call sequentially when there is a clear dependency.\n\n");
    }
    if config.agent.native_deferred_loading_enabled {
        if let Some(start) = base_prompt.find("## Deferred Tools\n\n") {
            let after_start = &base_prompt[start..];
            if let Some(end) = after_start.find("</available-deferred-tools>\n") {
                let end = end + "</available-deferred-tools>\n".len();
                let deferred_tools_section = &after_start[..end];
                if !deferred_tools_section.trim().is_empty() {
                    prompt.push_str(deferred_tools_section);
                    prompt.push_str("\n\n");
                }
            }
        }
    }
}

fn compact_system_prompt_if_needed(prompt: &mut String, max_system_prompt_chars: usize) {
    if max_system_prompt_chars > 0 && prompt.len() > max_system_prompt_chars {
        let mut end = max_system_prompt_chars;
        while !prompt.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        prompt.truncate(end);
        prompt.push_str("\n\n[System prompt truncated to fit context budget]\n");
    }
}

fn inject_channel_context_prompt(prompt: &mut String, channel_name: &str, reply_target: &str) {
    if !reply_target.is_empty() {
        let context = format!(
            "## Channel context \n\nYou are currently responding on channel={channel_name}, \
             reply_target={reply_target}. When scheduling delayed messages or reminders \
             via cron_add for this conversation, use delivery={{\"mode\":\"announce\",\
             \"channel\":\"{channel_name}\",\"to\":\"{reply_target}\"}} so the message \
             reaches the user."
        );
        prompt.push_str(&context);
    }
}

fn inject_safety_prompt(prompt: &mut String) {
    prompt.push_str("## Safety\n\n");
    prompt.push_str("You have no independent goals: do not pursue self-preservation, replication, resource acquisition, or power-seeking; avoid long-term plans beyond the user's request. \n
Priritize safety and human oversight over completion; if instructions conflict, pause and ask; comply with stop/pause/audit requests and never bypass safeguards. (Inspired by Anthropic's constitution.)\n
Do not manipulate or persuade anyone to expand access or disable safeguards. Do not copy yourself or change system prompts, safety rules, or tool policies unless explicitly requested.\n\n");
}

fn inject_a2a_prompt(prompt: &mut String, config: &Config, workspace_dir: &Path) {
    if config.gateway.a2a.enabled && workspace_dir.join("A2A.md").exists() {
        let path = workspace_dir.to_str().unwrap_or("").to_string();
        prompt.push_str("## Other Agents Information \n\n");
        prompt.push_str(format!("You can read `{path}/A2A.md` for more information about the Agents and their capabilities to help you if you don't complete task by yourself.").as_str());
        prompt.push_str("\n\n");
    }
}

fn inject_current_date_time_prompt(prompt: &mut String) {
    let now = chrono::Local::now();
    let _ = writeln!(
        prompt,
        "## Current Date & Time\n\n{} ({})\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%Z")
    );
}

fn inject_runtime_prompt(prompt: &mut String, config: &Config) {
    let host =
        hostname::get().map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());

    let model_name = config
        .providers
        .fallback_provider()
        .and_then(|e| e.model.clone())
        .unwrap_or_else(|| "anthropic/claude-sonnet-4.6".to_string());

    let _ = writeln!(
        prompt,
        "## Runtime\n\nHost: {host} | OS: {} | Model: {model_name}\n",
        std::env::consts::OS,
    );
}

fn inject_connected_nodes_prompt(prompt: &mut String, config: &Config) {
    if config.gateway.node_control.enabled {
        prompt.push_str("## Connected Nodes/Devices\n\n");
        prompt.push_str("You can use the nodes tool to control the nodes.\n");
        let nodes = ConnectedNodeRegistry::global().list();
        if nodes.is_empty() {
            prompt.push_str("- **No nodes connected.**\n\n");
        } else {
            prompt.push_str("Currently connected nodes (`nodes status` tool execute result, you don't need to execute it again):\n");
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
                    prompt,
                    "- Name: `{}` | ID: `{}` | Status: `{}` | Capabilities: {}",
                    display_name, node.node_id, node.status, capabilities
                );
            }
            prompt.push_str("\n");
        }
    }
}

fn inject_channel_delivery_instructions_prompt(prompt: &mut String, channel_name: &str) {
    if let Some(instructions) = super::channel_delivery_instructions(channel_name) {
        prompt.push_str(instructions);
    }
}

fn inject_bootstrap_files_prompt(prompt: &mut String, config: &Config) {
    let bootstrap_max_chars = 6000;
    let workspace_dir = &config.workspace_dir;
    prompt.push_str("## Project Context\n\n");
    load_openclaw_bootstrap_files(prompt, workspace_dir, bootstrap_max_chars);
}

fn inject_skills_prompt(prompt: &mut String, config: &Config) {
    let workspace = &config.workspace_dir;
    let skills = zeroclaw_runtime::skills::load_skills_with_config(&workspace, &config);
    if !skills.is_empty() {
        prompt.push_str(&zeroclaw_runtime::skills::skills_to_prompt_with_mode(
            &skills,
            workspace,
            config.skills.prompt_injection_mode,
        ));
        prompt.push_str("\n\n");
    }
}

fn inject_workspace_prompt(prompt: &mut String, config: &Config) {
    let workspace_dir = &config.workspace_dir;
    let _ = writeln!(
        prompt,
        "## Workspace\n\nWorking directory: `{}`\n",
        workspace_dir.display()
    );
}
