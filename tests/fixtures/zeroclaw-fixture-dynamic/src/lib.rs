//! Reference cdylib plugin for ZeroClaw — also a community-facing template.
//!
//! Demonstrates a complete dynamic plugin using the `#[zeroclaw_macros::plugin]`
//! attribute macro. The macro auto-generates the required FFI symbols
//! (`zc_api_version` and `zc_register_plugins`) and the `extern "C"` factory
//! wrapper, so the author only writes the Rust factory function.

use async_trait::async_trait;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_macros::plugin;

zeroclaw_api::tool_attribution!(FixtureEchoTool, zeroclaw_api::attribution::ToolKind::Plugin);

// ── The component itself ─────────────────────────────────────────────────────

/// Tool that echoes its JSON arguments back as text.
pub struct FixtureEchoTool;

#[async_trait]
impl Tool for FixtureEchoTool {
    fn name(&self) -> &str {
        "fixture-echo-dynamic"
    }

    fn description(&self) -> &str {
        "Dynamic plugin fixture: echoes input args as output. Demonstrates the cdylib plugin convention."
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

// ── Factory function ─────────────────────────────────────────────────────────

/// Rust factory function — the `#[plugin]` macro wraps this into the C ABI
/// factory and exports the two required FFI symbols automatically.
#[plugin(tool = "fixture-echo-dynamic")]
fn fixture_echo_factory() -> Box<dyn Tool> {
    Box::new(FixtureEchoTool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_export_matches_api_constant() {
        assert_eq!(zc_api_version(), zeroclaw_api::version::API_VERSION_U32);
    }

    #[test]
    fn factory_writes_non_null_box() {
        use core::ffi::c_void;
        let mut out: *mut c_void = core::ptr::null_mut();
        let rc = unsafe {
            __zc_factory_fixture_echo_factory(core::ptr::null(), 0, &mut out as *mut *mut c_void)
        };
        assert_eq!(rc, 0);
        assert!(!out.is_null());
        // Reclaim ownership and drop to avoid leaking.
        let _: Box<Box<dyn Tool>> = unsafe { Box::from_raw(out as *mut Box<dyn Tool>) };
    }
}
