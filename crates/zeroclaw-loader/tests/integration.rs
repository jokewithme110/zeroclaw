//! End-to-end tests for the ZeroClaw native dynamic plugin loader.
//!
//! Tests load compiled `.so` / `.dylib` / `.dll` fixtures from the workspace
//! `target/<profile>/` directory. Because `cargo test` does not automatically
//! build the `cdylib` output of dev-dependencies, the helpers below build a
//! fixture on demand if its library file is missing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeroclaw_api::error::ZeroClawError;
use zeroclaw_api::plugin::{PluginRegistry, RegistrySet};
use zeroclaw_loader::DynPluginLoader;

fn workspace_target_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../crates/zeroclaw-loader
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .join("target")
}

/// Build a fixture crate's cdylib if it is not already present.
///
/// `cargo test` compiles dev-dependencies as rlibs for the test binary, but
/// does not automatically emit the separate `cdylib` artifact that the loader
/// needs. This helper invokes `cargo build -p <crate>` when the expected
/// library file is missing.
fn ensure_fixture_built(crate_name: &str, expected_path: &Path) {
    if expected_path.exists() {
        return;
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR");

    let status = std::process::Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg(crate_name)
        .current_dir(workspace_root)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo build for {crate_name}: {e}"));

    assert!(
        status.success(),
        "cargo build -p {crate_name} failed with status {status:?}"
    );
    assert!(
        expected_path.exists(),
        "cargo build -p {crate_name} did not produce {}",
        expected_path.display()
    );
}

fn fixture_so_path() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let filename = if cfg!(target_os = "windows") {
        "zeroclaw_fixture_dynamic.dll"
    } else if cfg!(target_os = "macos") {
        "libzeroclaw_fixture_dynamic.dylib"
    } else {
        "libzeroclaw_fixture_dynamic.so"
    };
    let path = workspace_target_dir().join(profile).join(filename);
    ensure_fixture_built("zeroclaw-fixture-dynamic", &path);
    path
}

fn write_manifest_for_fixture() -> tempfile::TempDir {
    let so_path = fixture_so_path();
    let tmp = tempfile::tempdir().expect("create temp dir");
    let manifest_path = tmp.path().join("plugin.toml");
    let mut f = std::fs::File::create(&manifest_path).unwrap();
    writeln!(
        f,
        r#"
name = "zeroclaw-fixture-dynamic"
version = "0.1.0"
api_version = 100
library = "{}"

[[components]]
kind = "tool"
names = ["fixture-echo-dynamic"]
"#,
        so_path.display()
    )
    .unwrap();
    tmp
}

#[tokio::test]
async fn loads_fixture_and_executes_registered_tool() {
    let tmp = write_manifest_for_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    let loaded = DynPluginLoader::load(&manifest_path, &registries)
        .expect("fixture should load successfully");

    assert_eq!(loaded.manifest().name, "zeroclaw-fixture-dynamic");
    assert!(registries.tools.contains("fixture-echo-dynamic"));

    let tool = registries
        .tools
        .get("fixture-echo-dynamic", &serde_json::Value::Null)
        .expect("registered factory should produce instance");

    let result = tool
        .execute(serde_json::json!({ "msg": "hello-from-loader" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("hello-from-loader"));
    assert!(result.error.is_none());
}

#[test]
fn missing_library_path_returns_dynlibload() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("plugin.toml");
    let mut f = std::fs::File::create(&manifest_path).unwrap();
    writeln!(
        f,
        r#"
name = "ghost-plugin"
version = "0.1.0"
api_version = 100
library = "/this/path/does/not/exist.so"
"#
    )
    .unwrap();

    let registries = RegistrySet::new();
    let err = DynPluginLoader::load(&manifest_path, &registries).expect_err("should fail");
    let zc = err
        .downcast_ref::<ZeroClawError>()
        .expect("error should be ZeroClawError");
    assert!(
        matches!(zc, ZeroClawError::DynLibLoad { .. }),
        "got: {zc:?}"
    );
}

#[tokio::test]
async fn registry_arc_keeps_so_alive_after_loaded_plugin_drop() {
    // Verifies the lifecycle invariant: dropping LoadedPlugin while Bridge
    // closures still hold Arc<Library> clones must NOT unload the .so.
    let tmp = write_manifest_for_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    {
        let loaded =
            DynPluginLoader::load(&manifest_path, &registries).expect("load should succeed");
        assert_eq!(loaded.manifest().name, "zeroclaw-fixture-dynamic");
        // loaded drops here — Arc count for Library decreases by 1, but the
        // Bridge closure inside `registries.tools` still holds 1 Arc, so the
        // .so remains mapped.
    }

    // Now invoke the factory through the registry. If the .so was unloaded,
    // this would segfault on the C ABI factory call. Reaching here is the
    // success criterion.
    let tool = registries
        .tools
        .get("fixture-echo-dynamic", &serde_json::Value::Null)
        .expect("factory still callable after LoadedPlugin drop");
    let result = tool
        .execute(serde_json::json!({ "msg": "post-drop" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("post-drop"));
}

// ═════════════════════════════════════════════════════════════════════════════
//  Multi-component fixture tests
// ═════════════════════════════════════════════════════════════════════════════

fn multi_fixture_so_path() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let filename = if cfg!(target_os = "windows") {
        "zeroclaw_fixture_multi.dll"
    } else if cfg!(target_os = "macos") {
        "libzeroclaw_fixture_multi.dylib"
    } else {
        "libzeroclaw_fixture_multi.so"
    };
    let path = workspace_target_dir().join(profile).join(filename);
    ensure_fixture_built("zeroclaw-fixture-multi", &path);
    path
}

fn write_manifest_for_multi_fixture() -> tempfile::TempDir {
    let so_path = multi_fixture_so_path();
    let tmp = tempfile::tempdir().expect("create temp dir");
    let manifest_path = tmp.path().join("plugin.toml");
    let mut f = std::fs::File::create(&manifest_path).unwrap();
    writeln!(
        f,
        r#"
name = "zeroclaw-fixture-multi"
version = "0.1.0"
api_version = 100
library = "{}"

[[components]]
kind = "tool"
names = ["multi-echo-tool", "multi-reverse-tool"]

[[components]]
kind = "provider"
names = ["multi-dummy-provider"]
"#,
        so_path.display()
    )
    .unwrap();
    tmp
}

#[tokio::test]
async fn loads_multi_component_fixture() {
    let tmp = write_manifest_for_multi_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    let loaded = DynPluginLoader::load(&manifest_path, &registries)
        .expect("multi-component fixture should load successfully");

    assert_eq!(loaded.manifest().name, "zeroclaw-fixture-multi");
    assert!(registries.tools.contains("multi-echo-tool"));
    assert!(registries.tools.contains("multi-reverse-tool"));
    assert!(registries.providers.contains("multi-dummy-provider"));
}

#[tokio::test]
async fn multi_component_echo_tool_executes() {
    let tmp = write_manifest_for_multi_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&manifest_path, &registries)
        .expect("multi-component fixture should load successfully");

    let tool = registries
        .tools
        .get("multi-echo-tool", &serde_json::Value::Null)
        .expect("echo tool factory should produce instance");

    let result = tool
        .execute(serde_json::json!({ "msg": "hello-from-multi" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("hello-from-multi"));
}

#[tokio::test]
async fn multi_component_reverse_tool_executes() {
    let tmp = write_manifest_for_multi_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&manifest_path, &registries)
        .expect("multi-component fixture should load successfully");

    let tool = registries
        .tools
        .get("multi-reverse-tool", &serde_json::Value::Null)
        .expect("reverse tool factory should produce instance");

    let result = tool
        .execute(serde_json::json!({ "text": "hello" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert_eq!(result.output, "olleh");
}

#[tokio::test]
async fn multi_component_provider_chat_works() {
    let tmp = write_manifest_for_multi_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&manifest_path, &registries)
        .expect("multi-component fixture should load successfully");

    let provider = registries
        .providers
        .get("multi-dummy-provider", &serde_json::Value::Null)
        .expect("provider factory should produce instance");

    let reply = provider
        .chat_with_system(None, "ping", "any-model", None)
        .await
        .expect("provider chat should not fail");
    assert!(reply.contains("dummy-provider-reply"));
    assert!(reply.contains("ping"));
}

#[tokio::test]
async fn registry_arc_keeps_multi_so_alive_after_loaded_plugin_drop() {
    let tmp = write_manifest_for_multi_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    {
        let loaded =
            DynPluginLoader::load(&manifest_path, &registries).expect("load should succeed");
        assert_eq!(loaded.manifest().name, "zeroclaw-fixture-multi");
    }

    let tool = registries
        .tools
        .get("multi-echo-tool", &serde_json::Value::Null)
        .expect("factory still callable after LoadedPlugin drop");
    let result = tool
        .execute(serde_json::json!({ "msg": "post-drop-multi" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("post-drop-multi"));
}

// ═════════════════════════════════════════════════════════════════════════════
//  Handwritten C-style fixture tests
// ═════════════════════════════════════════════════════════════════════════════

fn handwritten_fixture_so_path() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let filename = if cfg!(target_os = "windows") {
        "zeroclaw_fixture_handwritten.dll"
    } else if cfg!(target_os = "macos") {
        "libzeroclaw_fixture_handwritten.dylib"
    } else {
        "libzeroclaw_fixture_handwritten.so"
    };
    let path = workspace_target_dir().join(profile).join(filename);
    ensure_fixture_built("zeroclaw-fixture-handwritten", &path);
    path
}

fn write_manifest_for_handwritten_fixture() -> tempfile::TempDir {
    let so_path = handwritten_fixture_so_path();
    let tmp = tempfile::tempdir().expect("create temp dir");
    let manifest_path = tmp.path().join("plugin.toml");
    let mut f = std::fs::File::create(&manifest_path).unwrap();
    writeln!(
        f,
        r#"
name = "zeroclaw-fixture-handwritten"
version = "0.1.0"
api_version = 100
library = "{}"

[[components]]
kind = "tool"
names = ["handwritten-echo"]
"#,
        so_path.display()
    )
    .unwrap();
    tmp
}

#[tokio::test]
async fn loads_handwritten_c_style_fixture() {
    let tmp = write_manifest_for_handwritten_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    let loaded = DynPluginLoader::load(&manifest_path, &registries)
        .expect("handwritten fixture should load successfully");

    assert_eq!(loaded.manifest().name, "zeroclaw-fixture-handwritten");
    assert!(registries.tools.contains("handwritten-echo"));

    let tool = registries
        .tools
        .get("handwritten-echo", &serde_json::Value::Null)
        .expect("handwritten factory should produce instance");

    let result = tool
        .execute(serde_json::json!({ "msg": "hello-handwritten" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("hello-handwritten"));
}

// ═════════════════════════════════════════════════════════════════════════════
//  Coexistence: macro-generated and handwritten plugins in one RegistrySet
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn macro_and_handwritten_plugins_coexist() {
    let dynamic_tmp = write_manifest_for_fixture();
    let dynamic_manifest = dynamic_tmp.path().join("plugin.toml");

    let handwritten_tmp = write_manifest_for_handwritten_fixture();
    let handwritten_manifest = handwritten_tmp.path().join("plugin.toml");

    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&dynamic_manifest, &registries).expect("dynamic fixture should load");
    DynPluginLoader::load(&handwritten_manifest, &registries)
        .expect("handwritten fixture should load");

    assert!(registries.tools.contains("fixture-echo-dynamic"));
    assert!(registries.tools.contains("handwritten-echo"));

    let dynamic_tool = registries
        .tools
        .get("fixture-echo-dynamic", &serde_json::Value::Null)
        .unwrap();
    let dynamic_result = dynamic_tool
        .execute(serde_json::json!({ "source": "macro" }))
        .await
        .unwrap();
    assert!(dynamic_result.output.contains("macro"));

    let handwritten_tool = registries
        .tools
        .get("handwritten-echo", &serde_json::Value::Null)
        .unwrap();
    let handwritten_result = handwritten_tool
        .execute(serde_json::json!({ "source": "handwritten" }))
        .await
        .unwrap();
    assert!(handwritten_result.output.contains("handwritten"));
}

// ═════════════════════════════════════════════════════════════════════════════
//  Channel plugin fixture tests
// ═════════════════════════════════════════════════════════════════════════════

fn channel_fixture_so_path() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let filename = if cfg!(target_os = "windows") {
        "zeroclaw_fixture_channel.dll"
    } else if cfg!(target_os = "macos") {
        "libzeroclaw_fixture_channel.dylib"
    } else {
        "libzeroclaw_fixture_channel.so"
    };
    let path = workspace_target_dir().join(profile).join(filename);
    ensure_fixture_built("zeroclaw-fixture-channel", &path);
    path
}

fn write_manifest_for_channel_fixture() -> tempfile::TempDir {
    let so_path = channel_fixture_so_path();
    let tmp = tempfile::tempdir().expect("create temp dir");
    let manifest_path = tmp.path().join("plugin.toml");
    let mut f = std::fs::File::create(&manifest_path).unwrap();
    writeln!(
        f,
        r#"
name = "zeroclaw-fixture-channel"
version = "0.1.0"
api_version = 100
library = "{}"

[[components]]
kind = "channel"
names = ["fixture-echo-channel"]
"#,
        so_path.display()
    )
    .unwrap();
    tmp
}

#[tokio::test]
async fn loads_channel_plugin_fixture() {
    let tmp = write_manifest_for_channel_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    let loaded = DynPluginLoader::load(&manifest_path, &registries)
        .expect("channel fixture should load successfully");

    assert_eq!(loaded.manifest().name, "zeroclaw-fixture-channel");
    assert!(registries.channels.contains("fixture-echo-channel"));
}

#[tokio::test]
async fn channel_plugin_factory_produces_instance() {
    let tmp = write_manifest_for_channel_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&manifest_path, &registries)
        .expect("channel fixture should load successfully");

    let channel = registries
        .channels
        .get("fixture-echo-channel", &serde_json::Value::Null)
        .expect("channel factory should produce instance");

    assert_eq!(channel.name(), "fixture-echo-channel");
    assert!(channel.health_check().await);
}

#[tokio::test]
async fn channel_plugin_factory_with_config() {
    let tmp = write_manifest_for_channel_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    DynPluginLoader::load(&manifest_path, &registries)
        .expect("channel fixture should load successfully");

    let config = serde_json::json!({ "name": "custom-channel-name" });
    let channel = registries
        .channels
        .get("fixture-echo-channel", &config)
        .expect("channel factory should produce instance with config");

    assert_eq!(channel.name(), "custom-channel-name");
}

#[tokio::test]
async fn registry_arc_keeps_channel_so_alive_after_loaded_plugin_drop() {
    // Verifies the lifecycle invariant: dropping LoadedPlugin while Bridge
    // closures still hold Arc<Library> clones must NOT unload the .so.
    let tmp = write_manifest_for_channel_fixture();
    let manifest_path = tmp.path().join("plugin.toml");
    let registries = Arc::new(RegistrySet::new());

    {
        let loaded =
            DynPluginLoader::load(&manifest_path, &registries).expect("load should succeed");
        assert_eq!(loaded.manifest().name, "zeroclaw-fixture-channel");
        // loaded drops here — Arc count for Library decreases by 1, but the
        // Bridge closure inside `registries.channels` still holds 1 Arc, so the
        // .so remains mapped.
    }

    // Now invoke the factory through the registry. If the .so was unloaded,
    // this would segfault on the C ABI factory call. Reaching here is the
    // success criterion.
    let channel = registries
        .channels
        .get("fixture-echo-channel", &serde_json::Value::Null)
        .expect("factory still callable after LoadedPlugin drop");
    assert!(channel.health_check().await);
}
