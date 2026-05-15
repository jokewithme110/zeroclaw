//! Reference cdylib plugin for ZeroClaw — also a community-facing template.
//!
//! Demonstrates a complete dynamic plugin:
//! - Implements [`Tool`] for `FixtureEchoTool`.
//! - Exports `zc_api_version` (returns the API version constant).
//! - Exports `zc_register_plugins` (registers the tool factory via the
//!   host-provided [`PluginHandle`]).
//! - Provides a `*FactoryFn`-shaped `extern "C"` factory.
//!
//! Authors of real native plugins should mirror this structure.

use core::ffi::c_void;

use async_trait::async_trait;
use zeroclaw_api::plugin::PluginHandle;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::version::API_VERSION_U32;

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

// ── Factory function (matches ToolFactoryFn) ─────────────────────────────────

/// `ToolFactoryFn` for [`FixtureEchoTool`].
///
/// # Safety
///
/// Caller must satisfy the contract documented on
/// [`zeroclaw_api::plugin::ToolFactoryFn`]. In particular `out_tool` must be a
/// non-null writable `*mut *mut c_void`.
unsafe extern "C" fn fixture_echo_factory(
    _config_json: *const u8,
    _config_len: usize,
    out_tool: *mut *mut c_void,
) -> i32 {
    if out_tool.is_null() {
        return 1;
    }
    let boxed: Box<dyn Tool> = Box::new(FixtureEchoTool);
    // Outer Box converts the dyn-Tool fat pointer to a thin pointer for FFI.
    let outer: Box<Box<dyn Tool>> = Box::new(boxed);
    let raw = Box::into_raw(outer) as *mut c_void;
    // SAFETY: caller guarantees out_tool points to a writable slot.
    unsafe { *out_tool = raw };
    0
}

// ── FFI symbols (the two required exports) ──────────────────────────────────

/// Version probe — first call the host makes after `dlopen`.
///
/// # Safety
///
/// Pure read of a constant; no preconditions.
#[unsafe(no_mangle)]
pub extern "C" fn zc_api_version() -> u32 {
    API_VERSION_U32
}

/// Registration entry point — host invokes once after `zc_api_version` returns
/// a compatible version.
///
/// # Safety
///
/// `handle` must be a valid pointer to a host-owned [`PluginHandle`]. The host
/// guarantees this; plugins must not retain or dereference the pointer beyond
/// this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zc_register_plugins(handle: *mut PluginHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: precondition documented above.
    let h = unsafe { &*handle };
    let name = b"fixture-echo-dynamic";
    // SAFETY: name buffer lives for the static lifetime; register_tool's
    // contract permits the host to copy the bytes immediately and not retain
    // the pointer.
    unsafe {
        (h.register_tool)(h.inner, name.as_ptr(), name.len(), fixture_echo_factory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_export_matches_api_constant() {
        assert_eq!(zc_api_version(), API_VERSION_U32);
    }

    #[test]
    fn factory_writes_non_null_box() {
        let mut out: *mut c_void = core::ptr::null_mut();
        let rc =
            unsafe { fixture_echo_factory(core::ptr::null(), 0, &mut out as *mut *mut c_void) };
        assert_eq!(rc, 0);
        assert!(!out.is_null());
        // Reclaim ownership and drop to avoid leaking.
        let _: Box<Box<dyn Tool>> = unsafe { Box::from_raw(out as *mut Box<dyn Tool>) };
    }
}
