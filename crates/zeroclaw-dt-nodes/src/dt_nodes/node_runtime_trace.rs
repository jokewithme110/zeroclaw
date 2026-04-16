use crate::dt_nodes::handlers::InvokeOutcome;
use serde_json::{Value, json};
use zeroclaw_runtime::observability::runtime_trace;

#[derive(Clone, Copy)]
pub struct NodeTraceCtx<'a> {
    pub req_id: &'a str,
    pub node_id: &'a str,
}

pub fn ws_connected(node_id: &str, gateway_host: &str, gateway_port: u16, attempt: u64) {
    runtime_trace::record_event(
        "node_ws_connected",
        Some("node"),
        None,
        None,
        None,
        Some(true),
        None,
        json!({
            "node_id": node_id,
            "gateway_host": gateway_host,
            "gateway_port": gateway_port,
            "attempt": attempt,
        }),
    );
}

pub fn ws_session_ended(node_id: &str, kind: &str, detail: Option<&str>) {
    let msg = detail.map(short_message);
    runtime_trace::record_event(
        "node_ws_session_ended",
        Some("node"),
        None,
        None,
        None,
        None,
        msg.as_deref(),
        json!({ "node_id": node_id, "kind": kind }),
    );
}

pub fn invoke_started(ctx: &NodeTraceCtx<'_>, command: &str) {
    runtime_trace::record_event(
        "node_invoke_start",
        Some("node"),
        None,
        None,
        Some(ctx.req_id),
        None,
        None,
        json!({ "node_id": ctx.node_id, "command": command }),
    );
}

pub fn invoke_completed(ctx: &NodeTraceCtx<'_>, command: &str, outcome: &InvokeOutcome) {
    let (err_code, err_message) = outcome
        .error
        .as_ref()
        .map(|e| {
            let code = e.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
            let msg = e.get("message").and_then(|m| m.as_str()).unwrap_or("");
            (Some(code), Some(msg))
        })
        .unwrap_or((None, None));
    let message = err_message.map(short_message);
    runtime_trace::record_event(
        "node_invoke_complete",
        Some("node"),
        None,
        None,
        Some(ctx.req_id),
        Some(outcome.ok),
        message.as_deref(),
        json!({
            "node_id": ctx.node_id,
            "command": command,
            "error_code": err_code,
            "has_payload": outcome.payload_json.is_some(),
        }),
    );
}

const MAX_PARAMS_PREVIEW_BYTES: usize = 16_384;
const MAX_TRACE_ARGS: usize = 128;
const MAX_TRACE_ARG_BYTES: usize = 8192;

pub fn system_run_rejected(
    ctx: &NodeTraceCtx<'_>,
    phase: &str,
    detail: Option<Value>,
    command_argv: Option<&[String]>,
    params_preview: Option<&str>,
) {
    let mut map = serde_json::Map::new();
    map.insert("node_id".into(), json!(ctx.node_id));
    map.insert("phase".into(), json!(phase));
    map.insert("detail".into(), detail.unwrap_or(Value::Null));
    if let Some(argv) = command_argv {
        let (av, list_tr, el_tr, tot) = argv_json_for_trace(argv);
        map.insert("command_argv".into(), av);
        map.insert("command_argv_total".into(), json!(tot));
        map.insert("command_argv_list_truncated".into(), json!(list_tr));
        map.insert("command_argv_element_truncated".into(), json!(el_tr));
        map.insert("argc".into(), json!(tot));
        if let Some(p) = argv.first() {
            map.insert("program".into(), json!(p));
        }
    }
    if let Some(raw) = params_preview {
        map.insert(
            "params_preview".into(),
            json!(truncate_preview(raw, MAX_PARAMS_PREVIEW_BYTES)),
        );
    }
    runtime_trace::record_event(
        "node_system_run",
        Some("node"),
        None,
        None,
        Some(ctx.req_id),
        Some(false),
        Some(phase),
        Value::Object(map),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn system_run_executed(
    ctx: &NodeTraceCtx<'_>,
    argv: &[String],
    cwd_set: bool,
    cwd: Option<&str>,
    env_keys: usize,
    timeout_ms: i64,
    exit_code: Option<i32>,
    timed_out: bool,
    cmd_success: bool,
    truncated: bool,
    error: Option<&str>,
) {
    let ok = cmd_success && error.is_none();
    let msg = error.map(short_message);
    let (command_argv, argv_list_truncated, argv_element_truncated, argv_total) = argv_json_for_trace(argv);
    let program = argv.first().map(|s| s.as_str()).unwrap_or("");
    let mut payload = json!({
        "node_id": ctx.node_id,
        "phase": "executed",
        "program": program,
        "argc": argv_total,
        "command_argv": command_argv,
        "command_argv_total": argv_total,
        "command_argv_list_truncated": argv_list_truncated,
        "command_argv_element_truncated": argv_element_truncated,
        "cwd_set": cwd_set,
        "env_keys": env_keys,
        "timeout_ms": timeout_ms,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "truncated": truncated,
    });
    if let Some(c) = cwd {
        if let Value::Object(ref mut m) = payload {
            m.insert("cwd".into(), json!(c));
        }
    }
    runtime_trace::record_event(
        "node_system_run",
        Some("node"),
        None,
        None,
        Some(ctx.req_id),
        Some(ok),
        msg.as_deref(),
        payload,
    );
}

fn argv_json_for_trace(argv: &[String]) -> (Value, bool, bool, usize) {
    let total = argv.len();
    let list_truncated = total > MAX_TRACE_ARGS;
    let mut any_element_truncated = false;
    let take = total.min(MAX_TRACE_ARGS);
    let mut arr = Vec::with_capacity(take);
    for s in argv.iter().take(take) {
        let (piece, truncated) = truncate_trace_arg(s);
        any_element_truncated |= truncated;
        arr.push(Value::String(piece));
    }
    (
        Value::Array(arr),
        list_truncated,
        any_element_truncated,
        total,
    )
}

fn truncate_trace_arg(s: &str) -> (String, bool) {
    const SUFFIX: &str = "…(truncated)";
    if s.len() <= MAX_TRACE_ARG_BYTES {
        return (s.to_string(), false);
    }
    let budget = MAX_TRACE_ARG_BYTES.saturating_sub(SUFFIX.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}{}", &s[..end], SUFFIX), true)
}

fn truncate_preview(s: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "…(preview truncated)";
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let budget = max_bytes.saturating_sub(SUFFIX.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &s[..end], SUFFIX)
}

fn short_message(s: &str) -> String {
    const MAX: usize = 240;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
