use crate::dt_nodes::handlers::InvokeOutcome;
use crate::dt_nodes::node_runtime_trace::NodeTraceCtx;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const OUTPUT_CAP: usize = 200_000;

#[derive(Deserialize)]
struct SystemRunParams {
    #[serde(rename = "command")]
    #[serde(default)]
    command: Vec<String>,
    #[serde(rename = "rawCommand")]
    #[serde(default)]
    raw_command: Option<String>,
    #[serde(rename = "cwd")]
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "env")]
    #[serde(default)]
    env: Option<Value>,
    #[serde(rename = "timeoutMs")]
    #[serde(default)]
    timeout_ms: Option<i64>,
}

#[derive(serde::Serialize)]
struct RunResult {
    #[serde(rename = "exitCode")]
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(rename = "timedOut")]
    timed_out: bool,
    success: bool,
    stdout: String,
    stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    truncated: bool,
}

pub async fn handle_system_run(params_json: &str, trace: Option<&NodeTraceCtx<'_>>) -> InvokeOutcome {
    let parsed = match parse_params(params_json) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, "dt_nodes system.run: rejected (invalid params)");
            if let Some(ctx) = trace {
                crate::dt_nodes::node_runtime_trace::system_run_rejected(
                    ctx,
                    "invalid_params",
                    Some(err.clone()),
                    None,
                    Some(params_json.trim()),
                );
            }
            return InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(err),
            };
        }
    };
    match run_command(parsed, trace).await {
        Ok(run) => {
            let success = run.success;
            tracing::info!(
                success = run.success,
                timed_out = run.timed_out,
                exit_code = ?run.exit_code,
                truncated = run.truncated,
                spawn_err = run.error.is_some(),
                "dt_nodes system.run: subprocess finished"
            );
            let payload_json = serde_json::to_string(&run).unwrap_or_else(|_| "{}".to_string());
            InvokeOutcome {
                ok: success,
                payload_json: Some(payload_json),
                error: None,
            }
        }
        Err(err) => {
            tracing::warn!(?err, "dt_nodes system.run: failed before spawn");
            if let Some(ctx) = trace {
                let empty: &[String] = &[];
                crate::dt_nodes::node_runtime_trace::system_run_rejected(
                    ctx,
                    "no_command",
                    None,
                    Some(empty),
                    None,
                );
            }
            InvokeOutcome {
                ok: false,
                payload_json: None,
                error: Some(err),
            }
        }
    }
}

fn parse_params(params_json: &str) -> Result<SystemRunParams, Value> {
    let trimmed = params_json.trim();
    if trimmed.is_empty() {
        return Err(invalid_request("paramsJSON required"));
    }
    serde_json::from_str::<SystemRunParams>(trimmed)
        .map_err(|e| invalid_request(&format!("invalid paramsJSON: {}", e)))
}

fn invalid_request(msg: &str) -> Value {
    serde_json::json!({ "code": "INVALID_REQUEST", "message": msg })
}

async fn run_command(params: SystemRunParams, trace: Option<&NodeTraceCtx<'_>>) -> Result<RunResult, Value> {
    let mut argv = params.command;
    if argv.is_empty() {
        if let Some(raw) = params.raw_command.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let (shell, args) = shell_exec();
            let mut full = Vec::with_capacity(args.len() + 2);
            full.push(shell.to_string());
            full.extend(args.iter().map(|s| s.to_string()));
            full.push(raw.to_string());
            argv = full;
        }
    }
    if argv.is_empty() {
        tracing::warn!("dt_nodes system.run: command required (empty argv)");
        return Err(invalid_request("command required"));
    }
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    let cwd_set = params.cwd.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let env_keys = params
        .env
        .as_ref()
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0);
    if let Some(ref cwd) = params.cwd {
        if !cwd.trim().is_empty() {
            cmd.current_dir(cwd);
        }
    }
    if let Some(ref env) = params.env {
        if let Some(obj) = env.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    cmd.env(k, s);
                } else {
                    cmd.env(k, v.to_string());
                }
            }
        }
    }
    let timeout_ms = params.timeout_ms.unwrap_or(60_000);
    let timeout_ms = if timeout_ms <= 0 { 60_000 } else { timeout_ms };
    let cwd_trace = params.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
    tracing::info!(
        program = %argv[0],
        argc = argv.len(),
        cwd_set,
        env_keys,
        timeout_ms,
        "dt_nodes system.run: spawning subprocess"
    );
    let output = match timeout(Duration::from_millis(timeout_ms as u64), cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            tracing::warn!(program = %argv[0], error = %e, "dt_nodes system.run: subprocess failed to start");
            let run = RunResult {
                exit_code: None,
                timed_out: false,
                success: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e.to_string()),
                truncated: false,
            };
            trace_system_run_result(trace, &argv, cwd_set, cwd_trace, env_keys, timeout_ms, &run);
            return Ok(run);
        }
        Err(_) => {
            tracing::warn!(program = %argv[0], timeout_ms, "dt_nodes system.run: subprocess timed out");
            let run = RunResult {
                exit_code: None,
                timed_out: true,
                success: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some("command timeout".to_string()),
                truncated: false,
            };
            trace_system_run_result(trace, &argv, cwd_set, cwd_trace, env_keys, timeout_ms, &run);
            return Ok(run);
        }
    };
    let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut truncated = false;
    if stdout.len() > OUTPUT_CAP {
        stdout = format!("... (truncated) {}", &stdout[stdout.len() - OUTPUT_CAP..]);
        truncated = true;
    }
    if stderr.len() > OUTPUT_CAP {
        stderr = format!("... (truncated) {}", &stderr[stderr.len() - OUTPUT_CAP..]);
        truncated = true;
    }
    let run = RunResult {
        exit_code: Some(output.status.code().unwrap_or(-1)),
        timed_out: false,
        success: output.status.success(),
        stdout,
        stderr,
        error: None,
        truncated,
    };
    trace_system_run_result(trace, &argv, cwd_set, cwd_trace, env_keys, timeout_ms, &run);
    Ok(run)
}

fn trace_system_run_result(
    trace: Option<&NodeTraceCtx<'_>>,
    argv: &[String],
    cwd_set: bool,
    cwd: Option<&str>,
    env_keys: usize,
    timeout_ms: i64,
    run: &RunResult,
) {
    let Some(ctx) = trace else {
        return;
    };
    crate::dt_nodes::node_runtime_trace::system_run_executed(
        ctx,
        argv,
        cwd_set,
        cwd,
        env_keys,
        timeout_ms,
        run.exit_code,
        run.timed_out,
        run.success,
        run.truncated,
        run.error.as_deref(),
    );
}

fn shell_exec() -> (&'static str, [&'static str; 1]) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", ["/c"])
    } else {
        ("/bin/sh", ["-c"])
    }
}
