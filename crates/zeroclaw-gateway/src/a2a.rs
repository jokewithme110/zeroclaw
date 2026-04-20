//! A2A HTTP endpoints backed by `ra2a`.

use super::AppState;
use anyhow::Result;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use parking_lot::RwLock;
use ra2a::server::{AgentExecutor, Event, EventQueue, RequestContext, ServerState};
use ra2a::types::{AgentCapabilities, AgentCard, AgentSkill, Message, Part, Task, TaskState, TaskStatus};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use std::{future::Future, pin::Pin};
use zeroclaw_api::tool::ToolSpec;
use zeroclaw_config::schema::{A2aConfig, Config};

const METHOD_MESSAGE_STREAM: &str = "message/stream";
const METHOD_TASKS_RESUBSCRIBE: &str = "tasks/resubscribe";
const DEFAULT_A2A_AGENT_CARD_NAME: &str = "ZeroClaw A2A Agent";
const DEFAULT_A2A_AGENT_CARD_DESCRIPTION: &str =
    "ZeroClaw A2A entrypoint powered by ra2a (v0.3.0 integration)";

static A2A_SERVER_STATE: OnceLock<RwLock<Option<ra2a::server::ServerState>>> = OnceLock::new();

fn a2a_server_state_cell() -> &'static RwLock<Option<ra2a::server::ServerState>> {
    A2A_SERVER_STATE.get_or_init(|| RwLock::new(None))
}

fn current_a2a_server_state() -> Option<ra2a::server::ServerState> {
    a2a_server_state_cell().read().clone()
}

fn to_skill_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn skill_slug(skill: &zeroclaw_runtime::skills::Skill) -> String {
    if let Some(location) = &skill.location {
        if let Some(parent) = location.parent() {
            if let Some(name) = parent.file_name().and_then(|v| v.to_str()) {
                return name.to_ascii_lowercase();
            }
        }
    }
    to_skill_id(&skill.name)
}

fn build_agent_skills(a2a: &A2aConfig, workspace_dir: &Path, allow_scripts: bool) -> Vec<AgentSkill> {
    let configured: HashSet<String> = a2a
        .skills
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let filtered = !configured.is_empty();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    let skills_dir = workspace_dir.join("skills");
    let loaded = zeroclaw_runtime::skills::load_skills_from_directory(&skills_dir, allow_scripts);
    for skill in loaded {
        let slug = skill_slug(&skill);
        if slug == "a2a-setup" {
            continue;
        }
        let skill_id = to_skill_id(&slug);
        if skill_id.is_empty() || seen_ids.contains(&skill_id) {
            continue;
        }
        let name_key = skill.name.trim().to_ascii_lowercase();
        if filtered && !configured.contains(&slug) && !configured.contains(&name_key) && !configured.contains(&skill_id) {
            continue;
        }
        let card_skill = AgentSkill::new(skill_id.clone(), skill.name.clone(), skill.description.clone(), skill.tags.clone());
        seen_ids.insert(skill_id);
        result.push(card_skill);
    }
    for entry in &a2a.agent_skills {
        let raw_id = entry.id.trim();
        if raw_id.is_empty() {
            continue;
        }
        let skill_id = to_skill_id(raw_id);
        if skill_id.is_empty() || seen_ids.contains(&skill_id) {
            continue;
        }
        let mut card_skill = AgentSkill::new(
            skill_id.clone(),
            entry.name.clone(),
            entry.description.clone(),
            entry.tags.clone(),
        );
        if !entry.examples.is_empty() {
            card_skill = card_skill.with_examples(entry.examples.clone());
        }
        seen_ids.insert(skill_id);
        result.push(card_skill);
    }
    result
}

fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

fn configured_or_default(value: Option<&str>, fallback: &'static str) -> String {
    value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(fallback).to_string()
}

pub fn init(config: &Config, base_url: &str, _tool_specs: &[ToolSpec]) -> Result<()> {
    struct ZeroClawExecutor {
        config_template: Config,
    }
    impl AgentExecutor for ZeroClawExecutor {
        fn execute<'a>(
            &'a self,
            ctx: &'a RequestContext,
            queue: &'a EventQueue,
        ) -> Pin<Box<dyn Future<Output = ra2a::Result<()>> + Send + 'a>> {
            Box::pin(async move {
                let mut working = ctx.stored_task.clone().unwrap_or_else(|| Task::new(&ctx.task_id, &ctx.context_id));
                if let Some(message) = ctx.message.clone() {
                    working.history.push(message);
                }
                working.status = TaskStatus::new(TaskState::Working);
                queue.send(Event::Task(working))?;
                let input = ctx.message.as_ref().and_then(Message::text_content).unwrap_or_default();
                let output = zeroclaw_runtime::agent::process_message(self.config_template.clone(), input.trim(), Some(&ctx.context_id)).await;
                let mut task = ctx.stored_task.clone().unwrap_or_else(|| Task::new(&ctx.task_id, &ctx.context_id));
                if let Some(message) = ctx.message.clone() {
                    task.history.push(message);
                }
                match output {
                    Ok(answer) => {
                        let reply = Message::agent(vec![Part::text(answer)]).with_task_id(&ctx.task_id).with_context_id(&ctx.context_id);
                        task.history.push(reply.clone());
                        task.status = TaskStatus::with_message(TaskState::Completed, reply);
                    }
                    Err(error) => {
                        task.status = TaskStatus::failed(error.to_string());
                    }
                }
                queue.send(Event::Task(task))?;
                Ok(())
            })
        }
        fn cancel<'a>(
            &'a self,
            ctx: &'a RequestContext,
            queue: &'a EventQueue,
        ) -> Pin<Box<dyn Future<Output = ra2a::Result<()>> + Send + 'a>> {
            Box::pin(async move {
                let mut task = ctx.stored_task.clone().unwrap_or_else(|| Task::new(&ctx.task_id, &ctx.context_id));
                task.status = TaskStatus::new(TaskState::Canceled);
                queue.send(Event::Task(task))?;
                Ok(())
            })
        }
    }

    let mut card = AgentCard::new(
        configured_or_default(config.gateway.a2a.agent_card_name.as_deref(), DEFAULT_A2A_AGENT_CARD_NAME),
        join_url(base_url, "/a2a"),
    );
    card.description = configured_or_default(
        config.gateway.a2a.agent_card_description.as_deref(),
        DEFAULT_A2A_AGENT_CARD_DESCRIPTION,
    );
    card.version = env!("CARGO_PKG_VERSION").to_string();
    card.capabilities = AgentCapabilities {
        streaming: config.gateway.a2a.stream_enabled,
        state_transition_history: true,
        ..AgentCapabilities::default()
    };
    card.skills = build_agent_skills(&config.gateway.a2a, &config.workspace_dir, config.skills.allow_scripts);
    let server_state = ServerState::from_executor(ZeroClawExecutor { config_template: config.clone() }, card);
    *a2a_server_state_cell().write() = Some(server_state);
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/a2a/.well-known/agent-card.json", get(handle_a2a_agent_card))
        .route("/a2a", post(handle_a2a_rpc))
}

fn is_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    if !state.pairing.require_pairing() {
        return true;
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");
    state.pairing.is_authenticated(token)
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
        })),
    )
        .into_response()
}

fn not_enabled_response() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "A2A runtime is disabled in gateway.a2a.enabled"
        })),
    )
        .into_response()
}

fn rpc_method_name(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(ToOwned::to_owned))
}

fn is_streaming_method(method: &str) -> bool {
    matches!(method, METHOD_MESSAGE_STREAM | METHOD_TASKS_RESUBSCRIBE)
}

fn streaming_disabled_response(method: &str) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -32601, "message": format!("Method '{method}' is disabled on this server") },
            "id": serde_json::Value::Null
        })),
    )
        .into_response()
}

pub async fn handle_a2a_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    if !state.config.lock().gateway.a2a.enabled {
        return not_enabled_response();
    }
    if let Some(server_state) = current_a2a_server_state() {
        return ra2a::server::handle_agent_card(State(server_state))
            .await
            .into_response();
    }
    not_enabled_response()
}

pub async fn handle_a2a_rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }
    if !state.config.lock().gateway.a2a.enabled {
        return not_enabled_response();
    }
    if let Some(server_state) = current_a2a_server_state() {
        if let Some(method) = rpc_method_name(&body) {
            if is_streaming_method(&method) {
                if !state.config.lock().gateway.a2a.stream_enabled {
                    return streaming_disabled_response(&method);
                }
                return ra2a::server::handle_sse(State(server_state), headers, body).await;
            }
        }
        return ra2a::server::handle_jsonrpc(State(server_state), headers, body).await;
    }
    not_enabled_response()
}
