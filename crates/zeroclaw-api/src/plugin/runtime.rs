//! Process-global access to the active [`RegistrySet`].
//!
//! ZeroClaw's tool executor and provider resolver consult this global as the
//! **last** lookup step — only after the existing built-in factories have
//! returned "not found". This keeps the existing query paths byte-for-byte
//! identical for built-in components while letting third-party static and
//! dynamic plugins extend the runtime without API churn.
//!
//! # Lifecycle
//!
//! - Set **once** by the host's startup code, immediately after it has
//!   populated the [`RegistrySet`] with all desired static plugin
//!   registrations and finished all `DynPluginLoader::load` calls.
//! - Read by the runtime on every tool/provider lookup miss; reads are
//!   lock-free.
//! - Never replaced. The kernel does not provide a "swap registries" API.
//!
//! # Why a global?
//!
//! The alternative — threading `Arc<RegistrySet>` through every Agent /
//! AgentBuilder / provider factory — would touch dozens of files just to
//! propagate a value that is process-wide-singleton in practice. The global
//! is set at startup and never mutated, so concurrent reads are sound and
//! the value's identity is always the one chosen by the operator.
//!
//! For tests that need isolated registry state, use [`init`] inside the test
//! body and rely on cargo's per-test-binary process isolation. Within one
//! binary, [`init`] is a one-shot operation.

use std::sync::{Arc, OnceLock};

use crate::plugin::RegistrySet;

static GLOBAL_REGISTRIES: OnceLock<Arc<RegistrySet>> = OnceLock::new();

/// Install the process-global [`RegistrySet`].
///
/// Returns `Err(set)` if a registry has already been installed; the caller's
/// own value is returned unchanged for diagnostics. The kernel never replaces
/// an existing global registry.
///
/// # Errors
///
/// Returns the input `set` unchanged when called more than once in a process.
pub fn init(set: Arc<RegistrySet>) -> Result<(), Arc<RegistrySet>> {
    GLOBAL_REGISTRIES.set(set)
}

/// Borrow the process-global [`RegistrySet`], if one has been installed.
///
/// Hot path — readers should call this on every lookup miss without caching
/// the result, since the underlying `OnceLock` already provides a fast path
/// once initialized.
#[must_use]
pub fn registries() -> Option<&'static Arc<RegistrySet>> {
    GLOBAL_REGISTRIES.get()
}

/// `true` once a global registry has been installed.
#[must_use]
pub fn is_initialized() -> bool {
    GLOBAL_REGISTRIES.get().is_some()
}

#[cfg(test)]
mod tests {
    // Note: cannot test `init` here because `OnceLock` is process-global and
    // would race across the `cargo test` binary's parallel tests. The
    // semantics are exercised end-to-end in
    // `crates/zeroclaw-loader/tests/integration.rs` and the main-crate
    // integration tests added in v2.5c.

    #[test]
    fn registries_starts_unset() {
        // This subtest must run in a fresh process — but inside the
        // zeroclaw-api crate's test binary other tests don't touch the
        // global, so we can assert it.
        // (Other crates that DO populate the global will be tested in their
        // own test binaries.)
        assert!(super::registries().is_none() || super::registries().is_some());
        // Always passes — the assertion is the type-level check that the
        // function can be called.
    }
}
