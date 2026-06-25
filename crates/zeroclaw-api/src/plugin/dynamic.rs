//! Dynamic-library plugin contract (C ABI).
//!
//! This module defines the **protocol** that a host uses to load a plugin shipped
//! as a `.so` / `.dll` / `.dylib`. It contains type definitions only — no
//! `dlopen` logic, no dependency on `libloading`. The real loader lives in the
//! host crate; this module is the source of truth for the wire contract.
//!
//! # The two plugin symbols
//!
//! Every dynamic plugin must export these `#[no_mangle] extern "C"` functions,
//! with names given by [`DynPlugin::VERSION_SYMBOL`] and
//! [`DynPlugin::REGISTER_SYMBOL`]:
//!
//! ```rust,ignore
//! use zeroclaw_api::plugin::{PluginHandle, DynPlugin};
//! use zeroclaw_api::version::API_VERSION_U32;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn zc_api_version() -> u32 {
//!     API_VERSION_U32
//! }
//!
//! #[unsafe(no_mangle)]
//! pub unsafe extern "C" fn zc_register_plugins(handle: *mut PluginHandle) {
//!     let handle = unsafe { &*handle };
//!     let name = b"my-tool";
//!     unsafe {
//!         (handle.register_tool)(handle.inner, name.as_ptr(), name.len(), my_tool_factory);
//!     }
//! }
//!
//! unsafe extern "C" fn my_tool_factory(
//!     _config_json: *const u8,
//!     _config_len: usize,
//!     _out_tool: *mut *mut core::ffi::c_void,
//! ) -> i32 {
//!     // Build `Box<Box<dyn Tool>>`, write its raw pointer into `*out_tool`,
//!     // return 0 on success.
//!     0
//! }
//! ```
//!
//! # Why C ABI and not Rust ABI?
//!
//! The Rust ABI is unstable between compiler versions. By routing plugin
//! boundaries through `extern "C"` + `#[repr(C)]` only, a plugin built with a
//! different rustc can still interoperate with the host.
//!
//! # What crosses the boundary
//!
//! - **In**: raw UTF-8 byte slices (`*const u8` + `usize`) — never `&str`.
//! - **Out**: opaque trait-object pointers (`*mut c_void`) that the host
//!   reinterprets as `Box<Box<dyn Trait>>` and owns from that point on.
//! - **Never**: `String`, `Vec`, or any other Rust-ABI type across the boundary.

use core::ffi::c_void;

// ── Factory function signatures ───────────────────────────────────────────────

/// Factory signature for a [`ModelProvider`](crate::model_provider::ModelProvider) component.
///
/// `out_provider` receives a raw pointer to a heap-allocated
/// `Box<dyn ModelProvider>` (boxed twice so the caller can recover the wide pointer).
/// A return value of `0` indicates success; any other value is a plugin-defined
/// error code.
///
/// # Safety
///
/// The caller must guarantee `config_json` points to `config_len` valid UTF-8
/// bytes, and that `out_provider` is a writable `*mut *mut c_void`. On success,
/// the host takes ownership of the returned pointer.
pub type ProviderFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_provider: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`Tool`](crate::tool::Tool) component.
///
/// Same contract as [`ProviderFactoryFn`]; the out-pointer refers to a
/// `Box<dyn Tool>`.
pub type ToolFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_tool: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`Channel`](crate::channel::Channel) component.
pub type ChannelFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_channel: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`Memory`](crate::memory_traits::Memory) component.
pub type MemoryFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_memory: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`MemoryStrategy`](crate::memory_traits::MemoryStrategy) component.
///
/// Same contract as [`MemoryFactoryFn`]; the out-pointer refers to a
/// `Box<dyn MemoryStrategy>`.
pub type MemoryStrategyFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_strategy: *mut *mut c_void,
) -> i32;

/// Factory signature for an [`Observer`](crate::observability_traits::Observer)
/// component.
pub type ObserverFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_observer: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`RuntimeAdapter`](crate::runtime_traits::RuntimeAdapter)
/// component.
pub type RuntimeFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_runtime: *mut *mut c_void,
) -> i32;

/// Factory signature for a [`Peripheral`](crate::peripherals_traits::Peripheral)
/// component.
pub type PeripheralFactoryFn = unsafe extern "C" fn(
    config_json: *const u8,
    config_len: usize,
    out_peripheral: *mut *mut c_void,
) -> i32;

// ── Register-callback signatures ──────────────────────────────────────────────

/// Host-supplied callback used by plugins to register a factory of the given
/// component kind into a [`RegistrySet`](crate::plugin::RegistrySet).
///
/// The `handle` argument is the opaque `inner` pointer from
/// [`PluginHandle::inner`]. `name` points to `name_len` UTF-8 bytes.
///
/// # Safety
///
/// See [`PluginHandle`] for the boundary contract. Implementations must not
/// retain `name` past the call; they must copy it into owned storage.
pub type RegisterProviderFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: ProviderFactoryFn,
);

/// Host callback for registering a [`Tool`](crate::tool::Tool) factory.
pub type RegisterToolFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: ToolFactoryFn,
);

/// Host callback for registering a [`Channel`](crate::channel::Channel) factory.
pub type RegisterChannelFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: ChannelFactoryFn,
);

/// Host callback for registering a [`Memory`](crate::memory_traits::Memory) factory.
pub type RegisterMemoryFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: MemoryFactoryFn,
);

/// Host callback for registering a [`MemoryStrategy`](crate::memory_traits::MemoryStrategy) factory.
pub type RegisterMemoryStrategyFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: MemoryStrategyFactoryFn,
);

/// Host callback for registering an [`Observer`](crate::observability_traits::Observer) factory.
pub type RegisterObserverFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: ObserverFactoryFn,
);

/// Host callback for registering a [`RuntimeAdapter`](crate::runtime_traits::RuntimeAdapter) factory.
pub type RegisterRuntimeFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: RuntimeFactoryFn,
);

/// Host callback for registering a [`Peripheral`](crate::peripherals_traits::Peripheral) factory.
pub type RegisterPeripheralFn = unsafe extern "C" fn(
    handle: *mut c_void,
    name: *const u8,
    name_len: usize,
    factory: PeripheralFactoryFn,
);

// ── PluginHandle ──────────────────────────────────────────────────────────────

/// Host-supplied callback table, passed by pointer to
/// [`DynPlugin::REGISTER_SYMBOL`].
///
/// Plugin code calls the function pointers here to register component factories
/// with the host's registry set. `inner` is an opaque host-side pointer
/// (typically to a [`RegistrySet`](crate::plugin::RegistrySet)); plugins must
/// never dereference it directly — only pass it back through the callbacks.
///
/// # Layout
///
/// `#[repr(C)]` guarantees a stable field order across compilers. The struct
/// holds 9 pointer-sized fields.
#[repr(C)]
pub struct PluginHandle {
    /// Opaque host-side pointer. Plugins must not dereference.
    pub inner: *mut c_void,

    /// Register a Provider factory. See [`RegisterProviderFn`].
    pub register_provider: RegisterProviderFn,

    /// Register a Tool factory. See [`RegisterToolFn`].
    pub register_tool: RegisterToolFn,

    /// Register a Channel factory. See [`RegisterChannelFn`].
    pub register_channel: RegisterChannelFn,

    /// Register a Memory factory. See [`RegisterMemoryFn`].
    pub register_memory: RegisterMemoryFn,

    /// Register a MemoryStrategy factory. See [`RegisterMemoryStrategyFn`].
    pub register_memory_strategy: RegisterMemoryStrategyFn,

    /// Register an Observer factory. See [`RegisterObserverFn`].
    pub register_observer: RegisterObserverFn,

    /// Register a RuntimeAdapter factory. See [`RegisterRuntimeFn`].
    pub register_runtime: RegisterRuntimeFn,

    /// Register a Peripheral factory. See [`RegisterPeripheralFn`].
    pub register_peripheral: RegisterPeripheralFn,
}

// ── DynPlugin — canonical symbol names ────────────────────────────────────────

/// Canonical symbol names the host will `dlsym` from a plugin library.
///
/// This is not a trait users implement; it is a namespace of `const`s that
/// pins the wire protocol's symbol names. Both the host loader and the plugin
/// author reference the same constants to stay in sync.
pub trait DynPlugin {
    /// Name of the version-probe function exported by every plugin.
    ///
    /// Signature: `extern "C" fn() -> u32`, returning a
    /// [`API_VERSION_U32`](crate::version::API_VERSION_U32)-compatible value.
    const VERSION_SYMBOL: &'static str = "zc_api_version";

    /// Name of the registration entry point exported by every plugin.
    ///
    /// Signature: `unsafe extern "C" fn(handle: *mut PluginHandle)`.
    const REGISTER_SYMBOL: &'static str = "zc_register_plugins";
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;
    impl DynPlugin for Probe {}

    #[test]
    fn plugin_handle_size_is_nine_pointers() {
        assert_eq!(
            core::mem::size_of::<PluginHandle>(),
            9 * core::mem::size_of::<*const c_void>(),
        );
    }

    #[test]
    fn plugin_handle_is_repr_c_field_order() {
        // A freshly-zeroed PluginHandle would be UB to use (null fn pointers),
        // but we can at least assert that the first field is at offset 0.
        assert_eq!(core::mem::offset_of!(PluginHandle, inner), 0);
        assert!(
            core::mem::offset_of!(PluginHandle, register_provider)
                < core::mem::offset_of!(PluginHandle, register_peripheral)
        );
    }

    #[test]
    fn symbol_names_are_stable() {
        assert_eq!(Probe::VERSION_SYMBOL, "zc_api_version");
        assert_eq!(Probe::REGISTER_SYMBOL, "zc_register_plugins");
    }
}
