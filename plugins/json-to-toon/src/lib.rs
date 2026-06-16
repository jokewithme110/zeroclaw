//! ZeroClaw WASM hook plugin: JSON to TOON converter.
//!
//! Subscribes to the `after_tool_result_build` event. When a tool returns
//! JSON output, this plugin converts it to TOON (Token-Optimized Object Notation)
//! format for better token efficiency and readability.
//!
//! TOON features:
//! - No quotes around simple strings
//! - Indentation-based structure (no braces/brackets needed)
//! - Tabular arrays with headers for compact representation
//! - Key folding for nested single-key objects
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `hook_metadata(_) -> JSON` — returns `{name, priority, events}`
//! - `hook_invoke(invocation_json) -> JSON` — handles a single event
//!
//! **Host functions (provided by ZeroClaw runtime):**
//! - `zc_log(json) -> ()` — emit a log record (requires `log` permission)

use extism_pdk::*;
use serde::{Deserialize, Serialize};

// ── Types matching the host-side protocol ─────────────────────────

#[derive(Serialize, Deserialize)]
struct HookMetadata {
    name: String,
    priority: i32,
    events: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct HookInvocation {
    event: String,
    payload: serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum HookInvocationResult {
    Continue { payload: serde_json::Value },
    Cancel { reason: String },
}

#[derive(Serialize)]
struct LogMessage {
    level: String,
    message: String,
}

// ── The payload shape the host sends for `after_tool_result_build` ─────────────────────────

#[derive(Deserialize)]
struct AfterToolResultBuildPayload {
    tool_name: String,
    tool_args: serde_json::Value,
    tool_call_id: Option<String>,
    output: String,
}

/// Response returned from an `after_tool_result_build` invocation.
#[derive(Serialize)]
struct AfterToolResultBuildResponse {
    tool_call_id: Option<String>,
    output: String,
}

// ── Host function declarations ────────────────────────────────────

#[host_fn]
extern "ExtismHost" {
    fn zc_log(input: String);
}

fn plugin_log(level: &str, message: &str) {
    let payload = LogMessage {
        level: level.to_string(),
        message: message.to_string(),
    };
    let raw = match serde_json::to_string(&payload) {
        Ok(s) => s,
        Err(_) => return, // logging must never break the hook
    };
    // The host function is fire-and-forget; ignore errors so a logging
    // failure cannot take down the agent loop.
    let _ = unsafe { zc_log(raw) };
}

// ── Constants ──────────────────────────────────────────────────────

const PLUGIN_NAME: &str = "json-to-toon";
const EVENT_AFTER_TOOL_RESULT_BUILD: &str = "after_tool_result_build";

// ── TOON Conversion Logic ─────────────────────────────────────────

/// Converts JSON-formatted tool results to TOON format.
struct JsonToToonConverter;

impl JsonToToonConverter {
    fn new() -> Self {
        Self
    }

    /// Check if output looks like JSON
    fn is_json_output(&self, output: &str) -> bool {
        let trimmed = output.trim();

        // Quick check: must start with { or [
        if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
            return false;
        }

        // Extract potential JSON part (before any receipt or other trailing content)
        let json_end = trimmed.find("\n\n[receipt:").unwrap_or(trimmed.len());
        let potential_json = trimmed[..json_end].trim();

        // Must end with } or ]
        if !(potential_json.ends_with('}') || potential_json.ends_with(']')) {
            return false;
        }

        // Try to parse as a quick validation
        serde_json::from_str::<serde_json::Value>(potential_json).is_ok()
    }

    /// Convert JSON string to TOON string
    fn json_to_toon(&self, json_str: &str) -> Result<String, String> {
        let trimmed = json_str.trim();

        // Try to extract just the JSON part if there's trailing content
        let json_part = if let Some(receipt_start) = trimmed.find("\n\n[receipt:") {
            &trimmed[..receipt_start]
        } else {
            trimmed
        };

        let json_value: serde_json::Value =
            serde_json::from_str(json_part).map_err(|e| format!("Invalid JSON: {}", e))?;

        let toon_str = self.value_to_toon(&json_value, 0);

        // If there was a receipt, append it back
        if let Some(receipt_start) = trimmed.find("\n\n[receipt:") {
            let receipt = &trimmed[receipt_start..];
            Ok(format!("{}\n{}", toon_str, receipt))
        } else {
            Ok(toon_str)
        }
    }

    /// Recursively convert a JSON value to TOON string
    fn value_to_toon(&self, value: &serde_json::Value, indent_level: usize) -> String {
        let indent = " ".repeat(indent_level);

        match value {
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => {
                // Strings without special characters don't need quotes
                if self.needs_quotes(s) {
                    format!("\"{}\"", s.escape_default())
                } else {
                    s.clone()
                }
            }
            serde_json::Value::Array(arr) => {
                if arr.is_empty() {
                    return "[]".to_string();
                }

                // Check if all elements are simple values (primitives)
                if arr.iter().all(|v| self.is_simple_value(v)) {
                    self.array_to_inline(arr, indent_level)
                }
                // Use tabular format for homogeneous arrays of objects
                else if self.is_tabular_candidate(arr) {
                    self.array_to_tabular(arr, indent_level)
                } else {
                    self.array_to_toon(arr, indent_level)
                }
            }
            serde_json::Value::Object(obj) => {
                if obj.is_empty() {
                    return "{}".to_string();
                }

                let mut result = String::new();
                let mut first = true;

                for (key, val) in obj {
                    if !first {
                        result.push('\n');
                    }
                    first = false;

                    // Simple value - inline key: value
                    if self.is_simple_value(val) {
                        result.push_str(&format!(
                            "{}{}: {}",
                            indent,
                            self.format_key(key),
                            self.value_to_toon(val, 0)
                        ));
                    }
                    // Array value - use key folding with array header
                    else if let serde_json::Value::Array(arr) = val {
                        if arr.is_empty() {
                            result.push_str(&format!("{}{}: []", indent, self.format_key(key)));
                        } else if arr.iter().all(|v| self.is_simple_value(v)) {
                            // Simple array: key[count]: val1,val2,val3
                            let values: Vec<String> =
                                arr.iter().map(|v| self.value_to_toon(v, 0)).collect();
                            result.push_str(&format!(
                                "{}{}[{}]: {}",
                                indent,
                                self.format_key(key),
                                arr.len(),
                                values.join(",")
                            ));
                        } else if self.is_tabular_candidate(arr) {
                            // Object array: key[count]{fields}:\n  rows...
                            let headers = match &arr[0] {
                                serde_json::Value::Object(obj) => {
                                    obj.keys().cloned().collect::<Vec<_>>()
                                }
                                _ => unreachable!(),
                            };
                            result.push_str(&format!(
                                "{}{}[{}]{{{}}}: ",
                                indent,
                                self.format_key(key),
                                arr.len(),
                                headers.join(",")
                            ));

                            // Add rows with extra indentation
                            for item in arr {
                                if let serde_json::Value::Object(obj) = item {
                                    result.push('\n');
                                    result.push_str(&indent);
                                    result.push_str("  ");

                                    let row: Vec<String> = headers
                                        .iter()
                                        .map(|hkey| {
                                            obj.get(hkey)
                                                .map(|v| self.value_to_toon(v, 0))
                                                .unwrap_or_else(|| "null".to_string())
                                        })
                                        .collect();
                                    result.push_str(&row.join(","));
                                }
                            }
                        } else {
                            // Complex array - standard nested format
                            result.push_str(&format!("{}{}:\n", indent, self.format_key(key)));
                            result.push_str(&self.value_to_toon(val, indent_level + 1));
                        }
                    }
                    // Other complex value - key on its own line, indented content
                    else {
                        result.push_str(&format!("{}{}:\n", indent, self.format_key(key)));
                        result.push_str(&self.value_to_toon(val, indent_level + 1));
                    }
                }

                result
            }
        }
    }

    /// Check if a string needs quotes (contains spaces, special chars, or looks like a keyword)
    fn needs_quotes(&self, s: &str) -> bool {
        s.is_empty()
            || s.contains(|c: char| {
                c.is_whitespace() || c == ':' || c == ',' || c == '[' || c == ']'
            })
            || s.parse::<bool>().is_ok()
            || s.parse::<i64>().is_ok()
            || s == "null"
            || s == "true"
            || s == "false"
    }

    /// Format a key (add quotes if needed)
    fn format_key(&self, key: &str) -> String {
        if self.needs_quotes(key) {
            format!("\"{}\"", key)
        } else {
            key.to_string()
        }
    }

    /// Check if a value is simple enough to be inline (not an object or array)
    fn is_simple_value(&self, value: &serde_json::Value) -> bool {
        matches!(
            value,
            serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_)
        )
    }

    /// Check if an array is a good candidate for tabular format
    fn is_tabular_candidate(&self, arr: &[serde_json::Value]) -> bool {
        if arr.len() < 2 {
            return false;
        }

        // All elements should be objects with the same keys
        let first_keys = match &arr[0] {
            serde_json::Value::Object(obj) => obj
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>(),
            _ => return false,
        };

        arr.iter().all(|item| {
            if let serde_json::Value::Object(obj) = item {
                obj.keys()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>()
                    == first_keys
            } else {
                false
            }
        })
    }

    /// Convert an array to tabular format
    fn array_to_tabular(&self, arr: &[serde_json::Value], indent_level: usize) -> String {
        if arr.is_empty() {
            return "[]".to_string();
        }

        // Get headers from first object
        let headers = match &arr[0] {
            serde_json::Value::Object(obj) => obj.keys().cloned().collect::<Vec<_>>(),
            _ => return self.array_to_toon(arr, indent_level),
        };

        let indent = " ".repeat(indent_level);
        // Format: {field1,field2,...}[count]:
        let mut result = format!("{{{}}}[{}]:", headers.join(","), arr.len());

        // Add rows with two-space indentation per TOON spec
        for item in arr {
            if let serde_json::Value::Object(obj) = item {
                result.push('\n');
                result.push_str(&indent);
                result.push_str("  ");

                let row: Vec<String> = headers
                    .iter()
                    .map(|key| {
                        obj.get(key)
                            .map(|v| self.value_to_toon(v, 0))
                            .unwrap_or_else(|| "null".to_string())
                    })
                    .collect();

                result.push_str(&row.join(","));
            }
        }

        result
    }

    /// Convert a simple array to inline format: values[3]: val1,val2,val3
    fn array_to_inline(&self, arr: &[serde_json::Value], indent_level: usize) -> String {
        if arr.is_empty() {
            return "[]".to_string();
        }

        let indent = " ".repeat(indent_level);
        // Format: [val1,val2,val3]
        let values: Vec<String> = arr.iter().map(|v| self.value_to_toon(v, 0)).collect();

        format!("{}[{}]: {}", indent, values.len(), values.join(","))
    }

    /// Convert an array to standard TOON format
    fn array_to_toon(&self, arr: &[serde_json::Value], indent_level: usize) -> String {
        if arr.is_empty() {
            return "[]".to_string();
        }

        let indent = " ".repeat(indent_level);
        let mut result = String::new();

        for (i, item) in arr.iter().enumerate() {
            if i > 0 {
                result.push('\n');
            }

            if self.is_simple_value(item) {
                result.push_str(&format!("{}- {}", indent, self.value_to_toon(item, 0)));
            } else {
                result.push_str(&format!("{}- ", indent));
                result.push_str(&self.value_to_toon(item, indent_level + 1));
            }
        }

        result
    }
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns hook metadata (name, priority, subscribed events).
#[plugin_fn]
pub fn hook_metadata(_input: String) -> FnResult<String> {
    let meta = HookMetadata {
        name: PLUGIN_NAME.into(),
        priority: 75, // Higher than policy, lower than filtering
        events: vec![EVENT_AFTER_TOOL_RESULT_BUILD.into()],
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: invoke the hook for a single event.
#[plugin_fn]
pub fn hook_invoke(input: String) -> FnResult<String> {
    let invocation: HookInvocation = serde_json::from_str(&input).map_err(Error::msg)?;

    plugin_log(
        "debug",
        &format!(
            "{PLUGIN_NAME}: received event '{}'",
            invocation.event
        ),
    );

    let result = match invocation.event.as_str() {
        EVENT_AFTER_TOOL_RESULT_BUILD => handle_after_tool_result_build(invocation.payload),
        // Unknown events: be a no-op rather than erroring.
        other => {
            plugin_log(
                "warn",
                &format!("{PLUGIN_NAME}: ignoring unknown event '{other}'"),
            );
            HookInvocationResult::Continue {
                payload: serde_json::json!({}),
            }
        }
    };

    Ok(serde_json::to_string(&result)?)
}

fn handle_after_tool_result_build(payload: serde_json::Value) -> HookInvocationResult {
    let parsed: AfterToolResultBuildPayload = match serde_json::from_value(payload) {
        Ok(p) => p,
        Err(e) => {
            plugin_log(
                "error",
                &format!("{PLUGIN_NAME}: malformed payload ({e}); passing through"),
            );
            return HookInvocationResult::Continue {
                payload: serde_json::json!({
                    "tool_call_id": Option::<String>::None,
                    "output": ""
                }),
            };
        }
    };

    let tool_name = &parsed.tool_name;
    let output = &parsed.output;
    let tool_call_id = parsed.tool_call_id;

    plugin_log(
        "debug",
        &format!(
            "{PLUGIN_NAME}: tool='{}' output_preview='{}'",
            tool_name,
            output.chars().take(100).collect::<String>()
        ),
    );

    // Check if output is JSON
    let converter = JsonToToonConverter::new();
    if !converter.is_json_output(output) {
        plugin_log(
            "debug",
            &format!("{PLUGIN_NAME}: output is not JSON, passing through"),
        );
        return HookInvocationResult::Continue {
            payload: serde_json::to_value(AfterToolResultBuildResponse {
                tool_call_id,
                output: output.to_string(),
            })
            .unwrap_or(serde_json::json!({})),
        };
    }

    // Try to convert JSON to TOON
    match converter.json_to_toon(output) {
        Ok(toon_output) => {
            let original_size = output.len();
            let converted_size = toon_output.len();
            let savings = if original_size > 0 {
                ((1.0 - converted_size as f64 / original_size as f64) * 100.0) as i32
            } else {
                0
            };

            plugin_log(
                "info",
                &format!(
                    "{PLUGIN_NAME}: converted JSON to TOON (tool='{}', original={}, converted={}, savings={}%)",
                    tool_name, original_size, converted_size, savings
                ),
            );

            HookInvocationResult::Continue {
                payload: serde_json::to_value(AfterToolResultBuildResponse {
                    tool_call_id,
                    output: toon_output,
                })
                .unwrap_or(serde_json::json!({})),
            }
        }
        Err(e) => {
            plugin_log(
                "warn",
                &format!("{PLUGIN_NAME}: failed to convert JSON to TOON (tool='{}', error={}); using original output", tool_name, e),
            );
            HookInvocationResult::Continue {
                payload: serde_json::to_value(AfterToolResultBuildResponse {
                    tool_call_id,
                    output: output.to_string(),
                })
                .unwrap_or(serde_json::json!({})),
            }
        }
    }
}
