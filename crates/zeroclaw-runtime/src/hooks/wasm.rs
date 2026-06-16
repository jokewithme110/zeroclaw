//! Re-export of the WASM plugin hook bridge.
//!
//! `WasmHook` lives in `zeroclaw-plugins` and implements
//! [`HookHandler`](crate::hooks::HookHandler) directly — only the
//! `before_agent_reply` event invokes the plugin; every other event uses
//! the trait's default no-op. This module exists so downstream callers can
//! keep using `zeroclaw_runtime::hooks::wasm::WasmHook` instead of reaching
//! into the plugins crate.

pub use zeroclaw_plugins::wasm_hook::WasmHook;
