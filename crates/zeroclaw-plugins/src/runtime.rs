//! Extism-based WASM execution bridge.
//!
//! Creates Extism plugin instances with permission-gated host functions
//! (`zc_http_request`, `zc_env_read`) and calls plugin-exported functions
//! (`tool_metadata`, `execute`).

use crate::PluginPermission;
use anyhow::{Context, Result};
use extism::PluginBuilder;
use extism::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use zeroclaw_api::tool::ToolResult;

// ── Host function context ─────────────────────────────────────────

/// Permissions + identity available to a single plugin invocation.
#[derive(Debug, Clone)]
struct HostContext {
    permissions: HashSet<PluginPermission>,
    /// Plugin name from the manifest. Attached to log records so plugin
    /// output is distinguishable from host output in the JSONL stream.
    plugin_name: String,
}

// ── Data types exchanged with plugins ─────────────────────────────

/// HTTP request sent from plugin to host via `zc_http_request`.
#[derive(Debug, Serialize, Deserialize)]
struct HttpRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// HTTP response returned from host to plugin.
#[derive(Debug, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

/// Log request sent from plugin to host via `zc_log`.
#[derive(Debug, Serialize, Deserialize)]
struct LogMessage {
    /// Level name (case-insensitive): "trace" | "debug" | "info" | "warn" |
    /// "error". Anything unrecognized falls back to `info`. `None`/missing
    /// also defaults to `info`.
    #[serde(default)]
    level: Option<String>,
    /// The log message text. Required.
    message: String,
}

/// Tool metadata returned by the `tool_metadata` export.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// Hook metadata returned by the `hook_metadata` export.
#[derive(Debug, Serialize, Deserialize)]
pub struct HookMetadata {
    pub name: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub events: Vec<String>,
}

/// Generic hook invocation request sent to `hook_invoke`.
#[derive(Debug, Serialize, Deserialize)]
pub struct HookInvocation {
    pub event: String,
    pub payload: serde_json::Value,
}

/// Result returned by the `hook_invoke` export.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookInvocationResult {
    Continue { payload: serde_json::Value },
    Cancel { reason: String },
}

/// Result returned by the `execute` export.
#[derive(Debug, Serialize, Deserialize)]
struct PluginToolResult {
    success: bool,
    output: String,
    #[serde(default)]
    error: Option<String>,
}

// ── Host function implementations ─────────────────────────────────

fn handle_http_request(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<HostContext>,
) -> Result<(), Error> {
    let ctx = user_data.get()?;
    let ctx = ctx.lock().unwrap();

    if !ctx.permissions.contains(&PluginPermission::HttpClient) {
        return Err(Error::msg(
            "permission denied: plugin does not have 'http_client' permission",
        ));
    }

    // Read input string from WASM memory
    let request_json: String = plugin.memory_get_val(&inputs[0])?;

    let req: HttpRequest = serde_json::from_str(&request_json)
        .map_err(|e| Error::msg(format!("invalid HTTP request JSON: {e}")))?;

    // 120s ceiling covers legitimate slow cases: large file downloads and slow
    // model-inference endpoints (fal.ai image generation routinely takes 20-60s
    // on cold models). A per-plugin override or tighter default is a candidate
    // follow-up — see ADR-003 §"Known gaps". Note: this runs inside
    // spawn_blocking, so a stalled request holds a blocking-pool thread for
    // the full duration.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| Error::msg(format!("failed to create HTTP client: {e}")))?;

    let mut builder = match req.method.to_uppercase().as_str() {
        "GET" => client.get(&req.url),
        "POST" => client.post(&req.url),
        "PUT" => client.put(&req.url),
        "DELETE" => client.delete(&req.url),
        "PATCH" => client.patch(&req.url),
        "HEAD" => client.head(&req.url),
        other => {
            return Err(Error::msg(format!("unsupported HTTP method: {other}")));
        }
    };

    for (k, v) in &req.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    if let Some(body) = req.body {
        builder = builder.body(body);
    }

    let resp = builder
        .send()
        .map_err(|e| Error::msg(format!("HTTP request failed: {e}")))?;

    let status = resp.status().as_u16();
    let headers: std::collections::HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .text()
        .map_err(|e| Error::msg(format!("failed to read response body: {e}")))?;

    let response = HttpResponse {
        status,
        body,
        headers,
    };

    let response_json = serde_json::to_string(&response)
        .map_err(|e| Error::msg(format!("failed to serialize response: {e}")))?;

    plugin.memory_set_val(&mut outputs[0], response_json)?;

    Ok(())
}

fn handle_env_read(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<HostContext>,
) -> Result<(), Error> {
    let ctx = user_data.get()?;
    let ctx = ctx.lock().unwrap();

    if !ctx.permissions.contains(&PluginPermission::EnvRead) {
        return Err(Error::msg(
            "permission denied: plugin does not have 'env_read' permission",
        ));
    }

    let var_name: String = plugin.memory_get_val(&inputs[0])?;

    let value = std::env::var(&var_name)
        .map_err(|_| Error::msg(format!("environment variable '{var_name}' not set")))?;

    plugin.memory_set_val(&mut outputs[0], value)?;

    Ok(())
}

/// Emit a log record on behalf of the plugin. Gated by `PluginPermission::Log`.
///
/// Plugin input is a JSON string `{"level": "info|warn|error|debug|trace",
/// "message": "..."}` written into the shared linear memory. The level is
/// case-insensitive; unknown or missing levels fall back to `info`. Output is
/// fire-and-forget — no value is written back to plugin memory.
fn handle_log(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: UserData<HostContext>,
) -> Result<(), Error> {
    let ctx = user_data.get()?;
    let ctx = ctx.lock().unwrap();

    if !ctx.permissions.contains(&PluginPermission::Log) {
        return Err(Error::msg(
            "permission denied: plugin does not have 'log' permission",
        ));
    }

    let raw: String = plugin.memory_get_val(&inputs[0])?;
    let parsed: LogMessage = serde_json::from_str(&raw)
        .map_err(|e| Error::msg(format!("invalid log request JSON: {e}")))?;

    let level = parse_log_level(parsed.level.as_deref().unwrap_or(""));
    let plugin_name = ctx.plugin_name.clone();

    // Drop the lock before emitting the record so a downstream subscriber
    // that calls back into a host function doesn't deadlock on this Mutex.
    drop(ctx);

    // `record!` takes the level as a path-less identifier, so dispatch by
    // variant instead of threading the runtime value through.
    let event = ::zeroclaw_log::Event::new("plugin.log", ::zeroclaw_log::Action::Note)
        .with_attrs(::serde_json::json!({ "plugin": plugin_name }));
    match level {
        PluginLogLevel::Trace => ::zeroclaw_log::record!(TRACE, event, &parsed.message),
        PluginLogLevel::Debug => ::zeroclaw_log::record!(DEBUG, event, &parsed.message),
        PluginLogLevel::Info => ::zeroclaw_log::record!(INFO, event, &parsed.message),
        PluginLogLevel::Warn => ::zeroclaw_log::record!(WARN, event, &parsed.message),
        PluginLogLevel::Error => ::zeroclaw_log::record!(ERROR, event, &parsed.message),
    }

    Ok(())
}

/// Internal level enum so we don't have to plumb `tracing::Level` (which
/// isn't a direct dep) through. Maps 1:1 onto the variants that
/// `zeroclaw_log::record!` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginLogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Map a plugin-supplied level string (case-insensitive) to a
/// [`PluginLogLevel`]. Unknown / empty input falls back to `Info`.
fn parse_log_level(input: &str) -> PluginLogLevel {
    match input.to_ascii_lowercase().as_str() {
        "trace" => PluginLogLevel::Trace,
        "debug" => PluginLogLevel::Debug,
        "info" | "" | "notice" => PluginLogLevel::Info,
        "warn" | "warning" => PluginLogLevel::Warn,
        "error" | "err" => PluginLogLevel::Error,
        _ => PluginLogLevel::Info,
    }
}

// ── Plugin creation and invocation ────────────────────────────────

/// Create an Extism plugin from a WASM file with the given permissions.
///
/// `plugin_name` is the manifest-declared plugin identifier; it is stamped
/// onto log records emitted through the `zc_log` host function so plugin
/// output is distinguishable from host output in the JSONL stream.
pub fn create_plugin(
    wasm_path: &Path,
    permissions: &[PluginPermission],
    plugin_name: &str,
) -> Result<extism::Plugin> {
    let perm_set: HashSet<PluginPermission> = permissions.iter().cloned().collect();
    let ctx = UserData::new(HostContext {
        permissions: perm_set,
        plugin_name: plugin_name.to_string(),
    });

    let http_fn = Function::new(
        "zc_http_request",
        [PTR],
        [PTR],
        ctx.clone(),
        handle_http_request,
    );

    let env_fn = Function::new("zc_env_read", [PTR], [PTR], ctx.clone(), handle_env_read);

    let log_fn = Function::new("zc_log", [PTR], [], ctx, handle_log);

    let manifest = Manifest::new([Wasm::file(wasm_path)]);

    // Configure wasmtime compilation cache.
    // Uses ZEROCLAW_CONFIG_DIR/data/plugins/wasmtime-cache.toml as config file
    // and ZEROCLAW_CONFIG_DIR/data/plugins/cache as cache directory.
    // Disables cache if ZEROCLAW_CONFIG_DIR is not set or cache directory cannot be created.
    let mut builder = PluginBuilder::new(manifest)
        .with_wasi(true)
        .with_functions([http_fn, env_fn, log_fn]);

    if let Ok(config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let plugins_dir = std::path::PathBuf::from(&config_dir)
            .join("data")
            .join("plugins");
        let config_file = plugins_dir.join("wasmtime-cache.toml");
        let cache_dir = plugins_dir.join("wasmruntime");

        if let Ok(()) = std::fs::create_dir_all(&cache_dir) {
            let cache_toml = format!(
                r#"[cache]
directory = "{}"
"#,
                cache_dir.display()
            );
            if std::fs::write(&config_file, cache_toml).is_ok() {
                builder = builder.with_cache_config(&config_file);
            }
        }
    }

    builder
        .build()
        .with_context(|| format!("failed to load WASM plugin from {}", wasm_path.display()))
}

/// Call the `tool_metadata` export and parse the result.
pub fn call_tool_metadata(plugin: &mut extism::Plugin) -> Result<ToolMetadata> {
    let output = plugin
        .call::<&str, String>("tool_metadata", "")
        .context("failed to call tool_metadata export")?;

    serde_json::from_str(&output).context("failed to parse tool_metadata JSON")
}

/// Call the `hook_metadata` export and parse the result.
pub fn call_hook_metadata(plugin: &mut extism::Plugin) -> Result<HookMetadata> {
    call_json_export(plugin, "hook_metadata", "")
}

/// Call the `execute` export with the given args JSON and return a `ToolResult`.
pub fn call_execute(plugin: &mut extism::Plugin, args_json: &[u8]) -> Result<ToolResult> {
    let input = std::str::from_utf8(args_json).context("plugin args are not valid UTF-8")?;

    let output = plugin
        .call::<&str, String>("execute", input)
        .context("failed to call plugin execute export")?;

    let result: PluginToolResult =
        serde_json::from_str(&output).context("failed to parse plugin execute result")?;

    Ok(ToolResult {
        success: result.success,
        output: result.output,
        error: result.error,
    })
}

/// Call the `hook_invoke` export with the given invocation JSON.
pub fn call_hook_invoke(
    plugin: &mut extism::Plugin,
    invocation_json: &[u8],
) -> Result<HookInvocationResult> {
    let input =
        std::str::from_utf8(invocation_json).context("hook invocation is not valid UTF-8")?;
    call_json_export(plugin, "hook_invoke", input)
}

fn call_json_export<T>(plugin: &mut extism::Plugin, export: &str, input: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let output = plugin
        .call::<&str, String>(export, input)
        .with_context(|| format!("failed to call {export} export"))?;
    serde_json::from_str(&output).with_context(|| format!("failed to parse {export} JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_context_permission_check() {
        let ctx = HostContext {
            permissions: HashSet::from([PluginPermission::HttpClient]),
            plugin_name: "test".into(),
        };
        assert!(ctx.permissions.contains(&PluginPermission::HttpClient));
        assert!(!ctx.permissions.contains(&PluginPermission::EnvRead));
        assert_eq!(ctx.plugin_name, "test");
    }

    #[test]
    fn log_message_serde_roundtrip() {
        let raw = r#"{"level":"warn","message":"something is off"}"#;
        let parsed: LogMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.level.as_deref(), Some("warn"));
        assert_eq!(parsed.message, "something is off");

        // Missing level field defaults to None (handler then maps to Info).
        let raw = r#"{"message":"hello"}"#;
        let parsed: LogMessage = serde_json::from_str(raw).unwrap();
        assert!(parsed.level.is_none());
        assert_eq!(parsed.message, "hello");
    }

    #[test]
    fn parse_log_level_maps_known_values_and_defaults_unknown() {
        assert_eq!(parse_log_level("trace"), PluginLogLevel::Trace);
        assert_eq!(parse_log_level("DEBUG"), PluginLogLevel::Debug);
        assert_eq!(parse_log_level("Info"), PluginLogLevel::Info);
        assert_eq!(parse_log_level("warn"), PluginLogLevel::Warn);
        assert_eq!(parse_log_level("WARNING"), PluginLogLevel::Warn);
        assert_eq!(parse_log_level("error"), PluginLogLevel::Error);
        assert_eq!(parse_log_level("err"), PluginLogLevel::Error);
        assert_eq!(parse_log_level("notice"), PluginLogLevel::Info);
        // Unknown / empty fall back to Info.
        assert_eq!(parse_log_level(""), PluginLogLevel::Info);
        assert_eq!(parse_log_level("loud"), PluginLogLevel::Info);
    }

    #[test]
    fn http_request_serde_roundtrip() {
        let req = HttpRequest {
            method: "POST".into(),
            url: "https://example.com/api".into(),
            headers: [("Authorization".into(), "Bearer tok".into())]
                .into_iter()
                .collect(),
            body: Some(r#"{"key":"value"}"#.into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.url, "https://example.com/api");
        assert_eq!(parsed.body.as_deref(), Some(r#"{"key":"value"}"#));
    }

    #[test]
    fn tool_metadata_serde() {
        let meta = ToolMetadata {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: ToolMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test_tool");
    }

    #[test]
    fn hook_metadata_serde() {
        let meta = HookMetadata {
            name: "policy-hook".into(),
            priority: 42,
            events: vec!["before_tool_call".into(), "on_after_tool_call".into()],
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: HookMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "policy-hook");
        assert_eq!(parsed.priority, 42);
        assert_eq!(parsed.events.len(), 2);
    }

    #[test]
    fn plugin_tool_result_serde() {
        let result = PluginToolResult {
            success: true,
            output: "hello".into(),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: PluginToolResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.output, "hello");
    }

    #[test]
    fn hook_invocation_result_serde() {
        let result = HookInvocationResult::Continue {
            payload: serde_json::json!({"name": "shell"}),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: HookInvocationResult = serde_json::from_str(&json).unwrap();
        match parsed {
            HookInvocationResult::Continue { payload } => {
                assert_eq!(payload["name"], "shell");
            }
            HookInvocationResult::Cancel { .. } => panic!("expected continue result"),
        }
    }

    #[test]
    fn missing_wasm_file_returns_error() {
        let result = create_plugin(Path::new("/nonexistent/plugin.wasm"), &[], "test-plugin");
        assert!(result.is_err());
    }

    /// Integration tests that load the actual image-gen WASM plugin.
    /// These require the plugin to be built first:
    ///   cd plugins/image-gen-fal && cargo build --target wasm32-wasip1 --release
    mod integration {
        use super::*;

        fn wasm_path() -> Option<std::path::PathBuf> {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../plugins/image-gen-fal/image_gen_fal.wasm");
            if path.exists() { Some(path) } else { None }
        }

        #[test]
        fn load_and_read_metadata() {
            let Some(path) = wasm_path() else {
                eprintln!("SKIP: image_gen_fal.wasm not found (build the plugin first)");
                return;
            };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let meta = call_tool_metadata(&mut plugin).unwrap();
            assert_eq!(meta.name, "image_gen_fal");
            assert!(meta.description.contains("image"));
            assert!(
                meta.parameters_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "prompt")
            );
        }

        #[test]
        fn execute_missing_prompt() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let args = serde_json::to_vec(&serde_json::json!({})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("prompt"));
        }

        #[test]
        fn execute_invalid_size() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "size": "bad"})).unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid size"));
        }

        #[test]
        fn execute_invalid_model_traversal() {
            let Some(path) = wasm_path() else { return };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let args =
                serde_json::to_vec(&serde_json::json!({"prompt": "test", "model": "../../evil"}))
                    .unwrap();
            let result = call_execute(&mut plugin, &args).unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("Invalid model"));
        }

        /// End-to-end: missing `FAL_API_KEY` exercises the `zc_env_read` host
        /// function — the host returns Err (var unset), which Extism propagates
        /// as a plugin-call trap. Proves the env_read path is wired.
        #[test]
        fn execute_missing_api_key_exercises_env_read_host_fn() {
            let Some(path) = wasm_path() else { return };
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("FAL_API_KEY") };
            let perms = vec![PluginPermission::HttpClient, PluginPermission::EnvRead];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("FAL_API_KEY") || msg.contains("not set"),
                "expected env-var error, got: {msg}"
            );
        }

        /// End-to-end permission enforcement: without `EnvRead`, the host
        /// function returns permission-denied and Extism propagates it as a trap.
        #[test]
        fn execute_without_env_read_permission_fails() {
            let Some(path) = wasm_path() else { return };
            // Only HttpClient granted — EnvRead missing
            let perms = vec![PluginPermission::HttpClient];
            let mut plugin = create_plugin(&path, &perms, "image-gen-fal").unwrap();
            let args = serde_json::to_vec(&serde_json::json!({"prompt": "a sunset"})).unwrap();
            let err = call_execute(&mut plugin, &args).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("permission") || msg.contains("env_read"),
                "expected permission-denied error, got: {msg}"
            );
        }
    }
}
