//! Plugin manifest — sidecar metadata distributed alongside a dynamic library.
//!
//! A manifest is read by the host **before** `dlopen`, so version mismatches
//! and permission violations can be rejected without ever loading code.
//! This module defines the data structures only; parsing (TOML, JSON, …) is
//! left to the caller, who can use any `serde`-compatible format.
//!
//! ```rust
//! use zeroclaw_api::plugin::manifest::{PluginManifest, PluginCapability};
//!
//! let json = r#"{
//!     "name": "my-tools",
//!     "version": "1.0.0",
//!     "api_version": 100,
//!     "components": [{ "kind": "tool", "names": ["search", "calculator"] }]
//! }"#;
//! let manifest: PluginManifest = serde_json::from_str(json).unwrap();
//! assert_eq!(manifest.components[0].kind, PluginCapability::Tool);
//! ```

use serde::{Deserialize, Serialize};

/// Metadata distributed alongside a `.so` / `.dll` / `.dylib`.
///
/// The host reads this before `dlopen`-ing the library. Use
/// [`check_compatibility`](crate::version::check_compatibility) on
/// [`PluginManifest::api_version`] to decide whether the plugin is
/// loadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin identifier (used as the prefix for registered component names if
    /// the host chooses to namespace them).
    pub name: String,

    /// Plugin version, human-readable (e.g. `"1.2.3"`).
    pub version: String,

    /// ZeroClaw API version the plugin was built against, encoded as
    /// `major * 10_000 + minor * 100 + patch`. Must satisfy
    /// [`check_compatibility`](crate::version::check_compatibility).
    pub api_version: u32,

    /// Optional long-form description.
    #[serde(default)]
    pub description: Option<String>,

    /// Relative path (from the manifest's directory) to the plugin library
    /// file. Hosts may ignore this and resolve the library by convention.
    #[serde(default)]
    pub library: Option<String>,

    /// Components the plugin declares it will register. The host may use this
    /// for UI listings and to detect mismatches against the actual
    /// registrations emitted by `zc_register_plugins`.
    #[serde(default)]
    pub components: Vec<ComponentSpec>,

    /// Sandbox permissions the plugin requires.
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,

    /// Optional base64-encoded Ed25519 signature over the library file.
    #[serde(default)]
    pub signature: Option<String>,

    /// Optional base64-encoded Ed25519 public key that produced `signature`.
    #[serde(default)]
    pub publisher_key: Option<String>,
}

/// One declared component group in a [`PluginManifest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSpec {
    /// Which kernel trait this group implements.
    pub kind: PluginCapability,

    /// Names to register under. Each name becomes a key in the matching
    /// registry inside [`RegistrySet`](crate::plugin::RegistrySet).
    pub names: Vec<String>,
}

/// Component kinds recognized by the kernel. Matches the seven registries on
/// [`RegistrySet`](crate::plugin::RegistrySet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    Provider,
    Tool,
    Channel,
    Memory,
    Observer,
    Runtime,
    Peripheral,
}

/// Sandbox permissions a plugin can request.
///
/// Hosts may reject plugins whose permissions exceed operator policy before
/// loading. The kernel itself does not enforce these; they are advisory
/// metadata consumed by the loader and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginPermission {
    /// Outbound HTTP to the listed host names (glob patterns allowed).
    HttpClient { allow_hosts: Vec<String> },
    /// Read/write access to the listed filesystem paths.
    FileSystem { allow_paths: Vec<String> },
    /// Read access to the listed environment variable names.
    Env { allow_keys: Vec<String> },
    /// Raw network access (TCP/UDP), intentionally coarse.
    Network,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_minimal_manifest() {
        let json = r#"{
            "name": "minimal",
            "version": "0.1.0",
            "api_version": 100
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.name, "minimal");
        assert_eq!(m.api_version, 100);
        assert!(m.components.is_empty());
        assert!(m.permissions.is_empty());
    }

    #[test]
    fn deserializes_full_manifest() {
        let json = r#"{
            "name": "full",
            "version": "1.2.3",
            "api_version": 100,
            "description": "demo",
            "library": "libfull.so",
            "components": [
                { "kind": "tool", "names": ["a", "b"] },
                { "kind": "provider", "names": ["p"] }
            ],
            "permissions": [
                { "kind": "http_client", "allow_hosts": ["api.example.com"] },
                { "kind": "network" }
            ],
            "signature": "sig",
            "publisher_key": "pk"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.components.len(), 2);
        assert_eq!(m.components[0].kind, PluginCapability::Tool);
        assert_eq!(m.components[1].kind, PluginCapability::Provider);
        assert_eq!(m.permissions.len(), 2);
        assert_eq!(m.signature.as_deref(), Some("sig"));
    }

    #[test]
    fn capability_roundtrip() {
        let src = [
            PluginCapability::Provider,
            PluginCapability::Tool,
            PluginCapability::Channel,
            PluginCapability::Memory,
            PluginCapability::Observer,
            PluginCapability::Runtime,
            PluginCapability::Peripheral,
        ];
        let json = serde_json::to_string(&src).unwrap();
        let back: Vec<PluginCapability> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn permission_tag_is_snake_case() {
        let p = PluginPermission::FileSystem {
            allow_paths: vec!["/tmp".into()],
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"kind\":\"file_system\""), "got: {s}");
    }
}
