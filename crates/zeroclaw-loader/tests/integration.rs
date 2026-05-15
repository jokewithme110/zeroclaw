//! End-to-end test: load `zeroclaw-fixture-dynamic` from its compiled `.so`,
//! exercise the registered tool, and verify lifecycle correctness.
//!
//! This test depends on `zeroclaw-fixture-dynamic` as a `dev-dependency`,
//! which makes Cargo build the cdylib output before running the test.
//! The cdylib lands in the workspace `target/<profile>/` directory; we
//! locate it by walking up from `CARGO_MANIFEST_DIR`.

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
    workspace_target_dir().join(profile).join(filename)
}

fn write_manifest_for_fixture() -> tempfile::TempDir {
    let so_path = fixture_so_path();
    assert!(
        so_path.exists(),
        "fixture cdylib not found at {} — cargo should have built it via dev-dependency",
        so_path.display()
    );
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
