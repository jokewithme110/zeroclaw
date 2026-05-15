//! Host-side loader for ZeroClaw native dynamic plugins.
//!
//! Reads a `plugin.toml`, validates API version, opens the matching `.so` /
//! `.dylib` / `.dll`, and lets the plugin populate the caller-supplied
//! [`RegistrySet`] through the C ABI defined in
//! [`zeroclaw_api::plugin::dynamic`].
//!
//! # Usage
//!
//! ```no_run
//! use std::path::Path;
//! use std::sync::Arc;
//! use zeroclaw_api::plugin::RegistrySet;
//! use zeroclaw_loader::DynPluginLoader;
//!
//! # fn main() -> anyhow::Result<()> {
//! let registries = Arc::new(RegistrySet::new());
//! let loaded = DynPluginLoader::load(Path::new("./my_plugin/plugin.toml"), &registries)?;
//! // `loaded` must outlive any factory call into the plugin's components.
//! drop(loaded);
//! # Ok(())
//! # }
//! ```
//!
//! # Errors
//!
//! All loader errors are returned as [`anyhow::Error`] with a structured
//! [`ZeroClawError`] inside. Tests can `downcast_ref::<ZeroClawError>()`
//! on the returned error to assert specific failure modes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use libloading::{Library, Symbol};
use zeroclaw_api::error::ZeroClawError;
use zeroclaw_api::plugin::{DynPlugin, PluginHandle, PluginManifest, RegistrySet};
use zeroclaw_api::version::{API_VERSION_U32, check_compatibility};

mod trampoline;

use trampoline::{
    LoaderContext, trampoline_register_channel, trampoline_register_memory,
    trampoline_register_observer, trampoline_register_peripheral, trampoline_register_provider,
    trampoline_register_runtime, trampoline_register_tool,
};

/// Entry point for loading a single dynamic plugin.
///
/// Stateless — every call is independent. Multiple plugins are loaded by
/// calling [`DynPluginLoader::load`] repeatedly, holding the returned
/// [`LoadedPlugin`] values until shutdown.
pub struct DynPluginLoader;

impl DynPluginLoader {
    /// Load the plugin described by the manifest at `manifest_path`, registering
    /// its components into `registries`.
    ///
    /// # Returns
    ///
    /// A [`LoadedPlugin`] handle. The caller **must** retain it for as long as
    /// any component produced by the plugin's factories may be used; dropping
    /// the handle is safe but only one of several Arc holders to the underlying
    /// library (see crate-level docs on `Arc<Library>` lifecycle).
    ///
    /// # Errors
    ///
    /// - `ZeroClawError::VersionMismatch` — manifest declares an incompatible
    ///   API version, or the loaded library reports one.
    /// - `ZeroClawError::DynLibLoad` — manifest cannot be read/parsed, or
    ///   `dlopen` fails.
    /// - `ZeroClawError::MissingSymbol` — library lacks `zc_api_version` or
    ///   `zc_register_plugins`.
    pub fn load(manifest_path: &Path, registries: &RegistrySet) -> anyhow::Result<LoadedPlugin> {
        let manifest = read_manifest(manifest_path)?;

        // ── 1. Pre-dlopen version check (cheap, avoids loading bad code) ────
        if let Err(msg) = check_compatibility(manifest.api_version) {
            return Err(ZeroClawError::VersionMismatch {
                message: format!("manifest at {}: {msg}", manifest_path.display()),
            }
            .into());
        }

        // ── 2. Resolve library file path ────────────────────────────────────
        let library_path = resolve_library_path(manifest_path, &manifest)?;

        // ── 3. dlopen ───────────────────────────────────────────────────────
        // SAFETY: Library::new is unsafe because the loaded code may execute
        // initializers. The plugin author is responsible for ensuring the
        // library is well-formed; this is the standard libloading contract.
        let library = unsafe {
            Library::new(&library_path).map_err(|e| ZeroClawError::DynLibLoad {
                path: library_path.display().to_string(),
                source: anyhow::Error::new(e),
            })?
        };
        let library = Arc::new(library);

        // ── 4. Post-dlopen version probe (defends against tampered library) ─
        let lib_path_str = library_path.display().to_string();
        let reported_version: u32 = {
            // SAFETY: dlsym on a valid Library; symbol type matches the C ABI
            // contract documented on DynPlugin::VERSION_SYMBOL.
            let sym: Symbol<unsafe extern "C" fn() -> u32> = unsafe {
                library
                    .get(<DynPluginProbe as DynPlugin>::VERSION_SYMBOL.as_bytes())
                    .map_err(|_| ZeroClawError::MissingSymbol {
                        path: lib_path_str.clone(),
                        symbol: <DynPluginProbe as DynPlugin>::VERSION_SYMBOL.to_string(),
                    })?
            };
            // SAFETY: plugin's zc_api_version is a pure read of a constant.
            unsafe { sym() }
        };
        if let Err(msg) = check_compatibility(reported_version) {
            return Err(ZeroClawError::VersionMismatch {
                message: format!(
                    "library at {lib_path_str} reports {reported_version}, host expects {API_VERSION_U32}: {msg}"
                ),
            }
            .into());
        }

        // ── 5. Build PluginHandle and call zc_register_plugins ──────────────
        let mut ctx = LoaderContext {
            registries: registries as *const RegistrySet,
            library: Arc::clone(&library),
        };
        let mut handle = PluginHandle {
            inner: &mut ctx as *mut _ as *mut core::ffi::c_void,
            register_provider: trampoline_register_provider,
            register_tool: trampoline_register_tool,
            register_channel: trampoline_register_channel,
            register_memory: trampoline_register_memory,
            register_observer: trampoline_register_observer,
            register_runtime: trampoline_register_runtime,
            register_peripheral: trampoline_register_peripheral,
        };

        {
            // SAFETY: dlsym on a valid Library; signature matches DynPlugin
            // contract.
            let register_fn: Symbol<unsafe extern "C" fn(*mut PluginHandle)> = unsafe {
                library
                    .get(<DynPluginProbe as DynPlugin>::REGISTER_SYMBOL.as_bytes())
                    .map_err(|_| ZeroClawError::MissingSymbol {
                        path: lib_path_str.clone(),
                        symbol: <DynPluginProbe as DynPlugin>::REGISTER_SYMBOL.to_string(),
                    })?
            };
            // SAFETY: handle points at a live LoaderContext on this stack
            // frame; trampolines are sound for the duration of this call.
            unsafe { register_fn(&mut handle as *mut PluginHandle) };
        }
        // From here on, the LoaderContext is no longer needed. Trampolines
        // have already produced Bridge closures that each hold their own
        // Arc<Library> clone.

        Ok(LoadedPlugin {
            manifest,
            _library: library,
        })
    }
}

/// Handle to a loaded dynamic plugin. Keep alive until shutdown.
///
/// The internal [`Arc<Library>`] keeps the `.so` mapped. Bridge closures stored
/// in [`RegistrySet`] each clone their own Arc — see crate-level docs.
#[derive(Debug)]
pub struct LoadedPlugin {
    /// Parsed manifest. Public for diagnostics and UI listings.
    pub manifest: PluginManifest,
    _library: Arc<Library>,
}

impl LoadedPlugin {
    /// Borrow the parsed manifest.
    #[must_use]
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

/// Scan one or more directories non-recursively for `*.toml` plugin manifests
/// and load each successfully-parsed one into `registries`.
///
/// Errors on individual plugins are logged at `warn` level and the scan
/// continues; a failed plugin never aborts the whole load. Returns the
/// successfully-loaded plugins, which the caller must keep alive for as long
/// as any factory in `registries` may be invoked (see crate-level docs on the
/// `Arc<Library>` lifecycle).
///
/// Directories that don't exist are skipped silently (logged at `debug`).
pub fn load_directories<I, P>(paths: I, registries: &RegistrySet) -> Vec<LoadedPlugin>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut loaded = Vec::new();
    for dir in paths {
        let dir = dir.as_ref();
        if !dir.exists() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                    .with_attrs(::serde_json::json!({ "path": dir.display().to_string() })),
                "plugin directory does not exist; skip"
            );
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "path": dir.display().to_string(),
                            "error": e.to_string(),
                        })),
                    "failed to read plugin directory; skip"
                );
                continue;
            }
        };
        for entry in entries.flatten() {
            let manifest_path = entry.path();
            if manifest_path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            match DynPluginLoader::load(&manifest_path, registries) {
                Ok(p) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "plugin": p.manifest.name.clone(),
                                "path": manifest_path.display().to_string(),
                            })),
                        "loaded native plugin"
                    );
                    loaded.push(p);
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "path": manifest_path.display().to_string(),
                                "error": e.to_string(),
                            })),
                        "failed to load plugin; skip"
                    );
                }
            }
        }
    }
    loaded
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Zero-sized probe to access [`DynPlugin`]'s default associated-constant values.
struct DynPluginProbe;
impl DynPlugin for DynPluginProbe {}

fn read_manifest(path: &Path) -> anyhow::Result<PluginManifest> {
    let text = std::fs::read_to_string(path).map_err(|e| ZeroClawError::DynLibLoad {
        path: path.display().to_string(),
        source: anyhow::Error::new(e).context("failed to read plugin manifest"),
    })?;
    let manifest: PluginManifest = toml::from_str(&text)
        .with_context(|| format!("failed to parse plugin manifest at {}", path.display()))
        .map_err(|e| ZeroClawError::DynLibLoad {
            path: path.display().to_string(),
            source: e,
        })?;
    Ok(manifest)
}

fn resolve_library_path(
    manifest_path: &Path,
    manifest: &PluginManifest,
) -> anyhow::Result<PathBuf> {
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if let Some(lib) = &manifest.library {
        let p = PathBuf::from(lib);
        if p.is_absolute() {
            return Ok(p);
        }
        return Ok(manifest_dir.join(p));
    }

    // Fallback: lib<name_normalized>.<ext> (Linux) / lib<...>.<dylib> / <...>.<dll>.
    let normalized = manifest.name.replace('-', "_");
    let filename = if cfg!(target_os = "windows") {
        format!("{normalized}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{normalized}.dylib")
    } else {
        format!("lib{normalized}.so")
    };
    Ok(manifest_dir.join(filename))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_manifest(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("plugin.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn version_mismatch_rejects_before_dlopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
name = "test-plugin"
version = "0.1.0"
api_version = 999999
"#,
        );
        let registries = RegistrySet::new();
        let err = DynPluginLoader::load(&path, &registries).expect_err("should fail");
        let zc = err
            .downcast_ref::<ZeroClawError>()
            .expect("error should be ZeroClawError");
        assert!(
            matches!(zc, ZeroClawError::VersionMismatch { .. }),
            "got: {zc:?}"
        );
    }

    #[test]
    fn missing_manifest_returns_dynlibload() {
        let registries = RegistrySet::new();
        let err = DynPluginLoader::load(Path::new("/nonexistent/path/plugin.toml"), &registries)
            .expect_err("should fail");
        let zc = err
            .downcast_ref::<ZeroClawError>()
            .expect("error should be ZeroClawError");
        assert!(
            matches!(zc, ZeroClawError::DynLibLoad { .. }),
            "got: {zc:?}"
        );
    }

    #[test]
    fn malformed_manifest_returns_dynlibload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(tmp.path(), "this is not = valid [[ toml");
        let registries = RegistrySet::new();
        let err = DynPluginLoader::load(&path, &registries).expect_err("should fail");
        let zc = err
            .downcast_ref::<ZeroClawError>()
            .expect("error should be ZeroClawError");
        assert!(
            matches!(zc, ZeroClawError::DynLibLoad { .. }),
            "got: {zc:?}"
        );
    }

    #[test]
    fn resolves_default_library_filename() {
        let manifest = PluginManifest {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION_U32,
            description: None,
            library: None,
            components: vec![],
            permissions: vec![],
            signature: None,
            publisher_key: None,
        };
        let resolved = resolve_library_path(Path::new("/some/dir/plugin.toml"), &manifest).unwrap();
        let expected_filename = if cfg!(target_os = "windows") {
            "my_plugin.dll"
        } else if cfg!(target_os = "macos") {
            "libmy_plugin.dylib"
        } else {
            "libmy_plugin.so"
        };
        assert_eq!(resolved, Path::new("/some/dir").join(expected_filename));
    }

    #[test]
    fn resolves_relative_library_against_manifest_dir() {
        let manifest = PluginManifest {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION_U32,
            description: None,
            library: Some("./libfoo.so".to_string()),
            components: vec![],
            permissions: vec![],
            signature: None,
            publisher_key: None,
        };
        let resolved = resolve_library_path(Path::new("/some/dir/plugin.toml"), &manifest).unwrap();
        assert_eq!(resolved, Path::new("/some/dir/./libfoo.so"));
    }

    #[test]
    fn resolves_absolute_library_unchanged() {
        let manifest = PluginManifest {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION_U32,
            description: None,
            library: Some("/abs/path/libx.so".to_string()),
            components: vec![],
            permissions: vec![],
            signature: None,
            publisher_key: None,
        };
        let resolved = resolve_library_path(Path::new("/some/dir/plugin.toml"), &manifest).unwrap();
        assert_eq!(resolved, Path::new("/abs/path/libx.so"));
    }
}
