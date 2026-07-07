//! Handwritten C-style dynamic plugin fixture for ZeroClaw.
//!
//! Demonstrates the raw dynamic-library ABI without using the `#[plugin]` macro.
//! This verifies that the host loader can load plugins that export the symbols
//! manually, using the same contract as macro-generated plugins.

use async_trait::async_trait;
use core::ffi::c_void;
use zeroclaw_api::tool::{Tool, ToolResult};

zeroclaw_api::tool_attribution!(
    HandwrittenEchoTool,
    zeroclaw_api::attribution::ToolKind::Plugin
);

// ── The component itself ─────────────────────────────────────────────────────

pub struct HandwrittenEchoTool;

#[async_trait]
impl Tool for HandwrittenEchoTool {
    fn name(&self) -> &str {
        "handwritten-echo"
    }

    fn description(&self) -> &str {
        "Handwritten C-style plugin fixture: echoes input args as output."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": true,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: args.to_string(),
            error: None,
        })
    }
}

// ── C ABI factory wrapper ────────────────────────────────────────────────────

unsafe extern "C" fn handwritten_factory(
    _config_json: *const u8,
    _config_len: usize,
    out_ptr: *mut *mut c_void,
) -> i32 {
    if out_ptr.is_null() {
        return 1;
    }
    let boxed: Box<dyn Tool> = Box::new(HandwrittenEchoTool);
    let outer: Box<Box<dyn Tool>> = Box::new(boxed);
    let raw = Box::into_raw(outer) as *mut c_void;
    // SAFETY: caller guarantees out_ptr points to a writable slot.
    unsafe { *out_ptr = raw };
    0
}

// ── Required exported symbols ────────────────────────────────────────────────

/// Version probe — first call the host makes after `dlopen`.
///
/// # Safety
/// Pure read of a constant; no preconditions.
#[unsafe(no_mangle)]
pub extern "C" fn zc_api_version() -> u32 {
    zeroclaw_api::version::API_VERSION_U32
}

/// Registration entry point — host invokes once after version check.
///
/// # Safety
/// `handle` must be a valid pointer to a host-owned `PluginHandle`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zc_register_plugins(handle: *mut zeroclaw_api::plugin::PluginHandle) {
    if handle.is_null() {
        return;
    }
    let h = unsafe { &*handle };
    let name = b"handwritten-echo";
    // SAFETY: name buffer is static; host copies bytes immediately.
    unsafe {
        (h.register_tool)(h.inner, name.as_ptr(), name.len(), handwritten_factory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_export_matches_api_constant() {
        assert_eq!(zc_api_version(), zeroclaw_api::version::API_VERSION_U32);
    }

    #[tokio::test]
    async fn handwritten_tool_round_trips_json() {
        let tool = HandwrittenEchoTool;
        let out = tool
            .execute(serde_json::json!({ "msg": "hello-handwritten" }))
            .await
            .unwrap();
        assert!(out.success);
        assert!(out.output.contains("hello-handwritten"));
    }
}
