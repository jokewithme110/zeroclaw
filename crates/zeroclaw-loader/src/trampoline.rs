//! Host-side trampolines that bridge plugin C ABI calls into [`RegistrySet`].
//!
//! When a plugin calls `(handle.register_tool)(handle.inner, name_ptr, len, factory)`,
//! the call lands in [`trampoline_register_tool`]. The trampoline:
//!
//! 1. Recovers the [`LoaderContext`] from `handle.inner`.
//! 2. Validates and copies the component name into owned `String` storage.
//! 3. Builds a Bridge closure that wraps the C ABI factory call in Rust's
//!    [`Factory`](zeroclaw_api::plugin::Factory) signature.
//! 4. Stores the closure in the matching registry on [`RegistrySet`].
//!
//! All seven trampolines share one body via the [`impl_trampoline!`] macro.
//!
//! # Safety
//!
//! These functions are called from foreign code. The caller (the plugin's
//! `zc_register_plugins`) must:
//! - pass a `handle` produced by [`crate::DynPluginLoader::load`] (i.e. a
//!   pointer to a [`LoaderContext`] produced by this crate);
//! - pass `name` / `name_len` describing a valid byte slice it owns for the
//!   duration of the call;
//! - pass a `factory` whose contract is documented on the matching
//!   `*FactoryFn` type alias in `zeroclaw_api::plugin::dynamic`.

use core::ffi::c_void;
use std::sync::Arc;

use libloading::Library;
use zeroclaw_api::plugin::{
    ChannelFactoryFn, MemoryFactoryFn, MemoryStrategyFactoryFn, ObserverFactoryFn,
    PeripheralFactoryFn, PluginRegistry, ProviderFactoryFn, RegistrySet, RuntimeFactoryFn,
    ToolFactoryFn,
};

/// Host-supplied context handed to every trampoline through `PluginHandle.inner`.
///
/// Lives on the stack of [`crate::DynPluginLoader::load`] for the duration of
/// the plugin's `zc_register_plugins` call. After that call returns, the
/// context is gone — but every Bridge closure stored in `RegistrySet` has
/// already cloned its own [`Arc<Library>`].
#[repr(C)]
pub(crate) struct LoaderContext {
    /// Borrowed pointer to the caller-owned [`RegistrySet`]. Valid only for
    /// the duration of `zc_register_plugins`.
    pub registries: *const RegistrySet,
    /// Shared library handle. Each Bridge closure clones one Arc to keep the
    /// `.so` mapped while the closure may still be invoked.
    pub library: Arc<Library>,
}

macro_rules! impl_trampoline {
    (
        fn $fn_name:ident,
        factory_ty = $factory_ty:ty,
        registry_field = $registry_field:ident,
        dyn_trait = $dyn_trait:path,
        kind_label = $kind_label:literal $(,)?
    ) => {
        /// Trampoline for the matching `Register*Fn` callback. See module docs.
        ///
        /// # Safety
        ///
        /// See module-level safety notes.
        pub(crate) unsafe extern "C" fn $fn_name(
            handle: *mut c_void,
            name: *const u8,
            name_len: usize,
            factory: $factory_ty,
        ) {
            if handle.is_null() {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "kind": $kind_label })),
                    "plugin called register_* with null handle; skip"
                );
                return;
            }
            // SAFETY: caller (DynPluginLoader::load) passed a pointer to a live
            // LoaderContext on its stack frame; the call is synchronous and
            // returns before the frame unwinds.
            let ctx = unsafe { &*(handle as *const LoaderContext) };

            let owned_name = if name.is_null() || name_len == 0 {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "kind": $kind_label })),
                    "plugin passed empty/null name for register_*; skip"
                );
                return;
            } else {
                const MAX_NAME_LEN: usize = 256;
                if name_len > MAX_NAME_LEN {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "kind": $kind_label,
                                "name_len": name_len,
                                "max_name_len": MAX_NAME_LEN,
                            })),
                        "plugin passed oversized name for register_*; skip"
                    );
                    return;
                }
                // SAFETY: caller documents the byte buffer is valid for the
                // call. We copy into an owned String immediately.
                let bytes = unsafe { core::slice::from_raw_parts(name, name_len) };
                match core::str::from_utf8(bytes) {
                    Ok(s) => s.to_owned(),
                    Err(_) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({ "kind": $kind_label })),
                            "plugin passed non-UTF8 name for register_*; skip"
                        );
                        return;
                    }
                }
            };

            let lib = Arc::clone(&ctx.library);
            let captured_name = owned_name.clone();
            // SAFETY: registries is non-null and valid for the duration of
            // zc_register_plugins (the caller of this trampoline).
            let registries = unsafe { &*ctx.registries };

            registries.$registry_field.register_factory(
                &owned_name,
                Arc::new(
                    move |config: &::serde_json::Value| -> ::anyhow::Result<Box<dyn $dyn_trait>> {
                        let json = ::serde_json::to_string(config)?;
                        let mut out: *mut c_void = core::ptr::null_mut();
                        // SAFETY: plugin's *FactoryFn contract — factory accepts
                        // a UTF-8 JSON byte slice and, on rc=0, writes a
                        // `Box<Box<dyn Trait>>::into_raw()` into *out.
                        let rc = unsafe {
                            factory(json.as_ptr(), json.len(), &mut out as *mut *mut c_void)
                        };
                        // Return code 10 is the agreed "plugin self-disabled"
                        // signal (e.g. a dynamic channel with `enabled = false`).
                        // Surface it with a stable marker prefix so callers can
                        // treat it as a non-error skip. Other non-zero codes are
                        // hard errors.
                        if rc == 10 {
                            ::anyhow::bail!(
                                "channel_disabled: plugin factory '{}' ({}) self-reported disabled (enabled = false)",
                                captured_name,
                                $kind_label
                            );
                        }
                        ::anyhow::ensure!(
                            rc == 0,
                            "plugin factory '{}' ({}): returned error code {rc}",
                            captured_name,
                            $kind_label
                        );
                        ::anyhow::ensure!(
                            !out.is_null(),
                            "plugin factory '{}' ({}): rc=0 but out pointer is null",
                            captured_name,
                            $kind_label
                        );
                        // SAFETY: per *FactoryFn contract, the plugin produced
                        // the pointer with `Box::into_raw(Box::new(box_trait))`.
                        let boxed: Box<Box<dyn $dyn_trait>> =
                            unsafe { Box::from_raw(out as *mut Box<dyn $dyn_trait>) };
                        // Each instantiated trait object must keep the .so
                        // mapped. Cloning into the closure scope is intentional:
                        // it survives even if the registry's closure is dropped
                        // mid-call (e.g. via shutdown race).
                        let _keep_lib = Arc::clone(&lib);
                        Ok(*boxed)
                    },
                ),
            );
        }
    };
}

impl_trampoline! {
    fn trampoline_register_provider,
    factory_ty = ProviderFactoryFn,
    registry_field = providers,
    dyn_trait = zeroclaw_api::model_provider::ModelProvider,
    kind_label = "provider",
}

impl_trampoline! {
    fn trampoline_register_tool,
    factory_ty = ToolFactoryFn,
    registry_field = tools,
    dyn_trait = zeroclaw_api::tool::Tool,
    kind_label = "tool",
}

impl_trampoline! {
    fn trampoline_register_channel,
    factory_ty = ChannelFactoryFn,
    registry_field = channels,
    dyn_trait = zeroclaw_api::channel::Channel,
    kind_label = "channel",
}

impl_trampoline! {
    fn trampoline_register_memory,
    factory_ty = MemoryFactoryFn,
    registry_field = memory,
    dyn_trait = zeroclaw_api::memory_traits::Memory,
    kind_label = "memory",
}

impl_trampoline! {
    fn trampoline_register_memory_strategy,
    factory_ty = MemoryStrategyFactoryFn,
    registry_field = memory_strategies,
    dyn_trait = zeroclaw_api::memory_traits::MemoryStrategy,
    kind_label = "memory_strategy",
}

impl_trampoline! {
    fn trampoline_register_observer,
    factory_ty = ObserverFactoryFn,
    registry_field = observers,
    dyn_trait = zeroclaw_api::observability_traits::Observer,
    kind_label = "observer",
}

impl_trampoline! {
    fn trampoline_register_runtime,
    factory_ty = RuntimeFactoryFn,
    registry_field = runtimes,
    dyn_trait = zeroclaw_api::runtime_traits::RuntimeAdapter,
    kind_label = "runtime",
}

impl_trampoline! {
    fn trampoline_register_peripheral,
    factory_ty = PeripheralFactoryFn,
    registry_field = peripherals,
    dyn_trait = zeroclaw_api::peripherals_traits::Peripheral,
    kind_label = "peripheral",
}
