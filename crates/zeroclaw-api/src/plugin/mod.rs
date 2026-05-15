//! Plugin infrastructure: factory registration, component retrieval, and the unified
//! [`RegistrySet`] that aggregates all 7 typed component registries.
//!
//! # Overview
//!
//! The plugin module provides a generic, thread-safe registry system for ZeroClaw's
//! microkernel architecture. Plugin authors register component factories; the runtime
//! retrieves instances by name using [`PluginRegistry::get`].
//!
//! # Quick start
//!
//! ```rust,ignore
//! use zeroclaw_api::plugin::{RegistrySet, PluginRegistry};
//!
//! let registries = RegistrySet::new();
//! registries.tools.register("my-tool", |_cfg| {
//!     Ok(Box::new(MyTool::new()))
//! });
//! let tool = registries.tools.get("my-tool", &serde_json::Value::Null)?;
//! ```
//!
//! # Third-party static plugin convention
//!
//! Authors of statically-linked third-party plugins (Rust crates compiled
//! into the host binary at build time, as opposed to `.so` / `.dylib` /
//! `.dll` files loaded at runtime) follow a single convention:
//!
//! - The plugin crate's library exposes one public function:
//!   `pub fn register(registries: &RegistrySet)`.
//! - Inside `register`, the plugin calls one or more
//!   `registries.<kind>.register(name, factory)` per component it provides.
//! - The host adds one line per static plugin to its startup code:
//!   `my_plugin_crate::register(&registries);`.
//!
//! ```rust,ignore
//! // Inside a third-party plugin crate (`my-plugin`):
//! use zeroclaw_api::plugin::{PluginRegistry, RegistrySet};
//!
//! pub fn register(registries: &RegistrySet) {
//!     registries.tools.register("my-tool", |cfg| {
//!         Ok(Box::new(MyTool::from_config(cfg)?))
//!     });
//! }
//! ```
//!
//! This convention has no library-level enforcement — it lives by example.
//! The reference implementation is the `zeroclaw-fixture-static` crate.
//! Native dynamic plugins (`.so`/`.dylib`/`.dll`) follow the C ABI defined
//! in [`dynamic`] instead.

pub mod dynamic;
pub mod manifest;
pub mod registry;
pub mod runtime;

pub use dynamic::{
    ChannelFactoryFn, DynPlugin, MemoryFactoryFn, ObserverFactoryFn, PeripheralFactoryFn,
    PluginHandle, ProviderFactoryFn, RegisterChannelFn, RegisterMemoryFn, RegisterObserverFn,
    RegisterPeripheralFn, RegisterProviderFn, RegisterRuntimeFn, RegisterToolFn, RuntimeFactoryFn,
    ToolFactoryFn,
};
pub use manifest::{ComponentSpec, PluginCapability, PluginManifest, PluginPermission};
pub use registry::{
    // Config aliases
    ChannelConfig,
    // Registry aliases
    ChannelRegistry,
    // Core types
    Factory,
    HashMapRegistry,
    MemoryConfig,
    MemoryRegistry,
    ObserverConfig,
    ObserverRegistry,
    PeripheralConfig,
    PeripheralRegistry,
    PluginRegistry,
    ProviderConfig,
    ProviderRegistry,
    RegistrySet,
    RuntimeConfig,
    RuntimeRegistry,
    ToolConfig,
    ToolRegistry,
};
