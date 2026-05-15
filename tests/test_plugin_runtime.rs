//! Plugin runtime integration tests.
//!
//! Combined surface for v2.5c verification: static plugin registration via the
//! convention, dynamic plugin loading via `zeroclaw-loader`, and the runtime's
//! fallback to the global plugin [`RegistrySet`] for tool lookups.
//!
//! These tests share a single test binary, which means the
//! `zeroclaw_api::plugin::runtime` global is set exactly once. Tests that
//! depend on the global use a `OnceLock`-guarded `init_for_tests` helper and
//! run in process-shared mode (i.e. they cooperatively populate one
//! `RegistrySet`, since the global cannot be replaced).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use zeroclaw_api::plugin::{PluginRegistry, RegistrySet, runtime as plugin_runtime};

/// Shared, process-wide registry initialized exactly once. Multiple tests
/// register their fixtures into this single registry; the global accessor
/// then sees the union.
static SHARED_REGISTRIES: OnceLock<Arc<RegistrySet>> = OnceLock::new();

fn shared_registries() -> &'static Arc<RegistrySet> {
    SHARED_REGISTRIES.get_or_init(|| {
        let registries = Arc::new(RegistrySet::new());
        // Static plugin: register the fixture's tools.
        zeroclaw_fixture_static::register(&registries);
        // Dynamic plugin: load the cdylib fixture (if present).
        if let Some(manifest_path) = dynamic_fixture_manifest() {
            // Load — failures here are non-fatal for the static-only tests
            // below; tests that strictly need the dynamic plugin assert
            // load success themselves via a second probe.
            if let Err(e) = zeroclaw_loader::DynPluginLoader::load(&manifest_path, &registries) {
                eprintln!("note: dynamic fixture load skipped: {e}");
            }
        }
        // Install global. First writer wins; subsequent calls would error,
        // but with OnceLock guarding initialization we get here exactly once.
        plugin_runtime::init(Arc::clone(&registries))
            .unwrap_or_else(|_| panic!("global plugin registries must be unset at first init"));
        registries
    })
}

fn workspace_target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target")
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

/// Write a temporary manifest pointing at the compiled fixture cdylib.
/// Returns the path to the manifest, plus a TempDir guard the caller must
/// keep alive to prevent cleanup.
///
/// Returns `None` if the cdylib is not present at the expected path (e.g.
/// the dev-dependency build hasn't run); tests that require the dynamic
/// plugin will skip themselves in that case.
fn dynamic_fixture_manifest() -> Option<PathBuf> {
    let so_path = fixture_so_path();
    if !so_path.exists() {
        eprintln!(
            "note: dynamic fixture not found at {} — skipping dynamic test setup",
            so_path.display()
        );
        return None;
    }
    // Use a per-test-binary file (not a TempDir) so the manifest survives
    // for the duration of the OnceLock initializer.
    let manifest_path = std::env::temp_dir().join(format!(
        "zeroclaw-test-plugin-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&manifest_path).ok()?;
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
    .ok()?;
    Some(manifest_path)
}

// ── v2.5a: static plugin convention ──────────────────────────────────────────

#[test]
fn static_plugin_register_populates_registry() {
    let registries = shared_registries();
    assert!(
        registries.tools.contains("fixture-echo-static"),
        "static fixture's tool should be registered as 'fixture-echo-static'"
    );
}

// ── v2.5b: dynamic plugin loading via DynPluginLoader ────────────────────────

#[tokio::test]
async fn dynamic_plugin_loaded_and_executable() {
    let registries = shared_registries();
    if !Path::new(&fixture_so_path()).exists() {
        eprintln!("skip: dynamic fixture cdylib not built");
        return;
    }
    // Static and dynamic fixtures now use distinct names, so both can coexist
    // in the same RegistrySet without one overwriting the other.
    let tool = registries
        .tools
        .get("fixture-echo-dynamic", &serde_json::Value::Null)
        .expect("dynamic plugin tool factory should be invokable");
    let result = tool
        .execute(serde_json::json!({ "msg": "from-test-binary" }))
        .await
        .expect("execute should not fail");
    assert!(result.success);
    assert!(result.output.contains("from-test-binary"));
}

// ── v2.5a+v2.5b: both fixtures coexist independently ────────────────────────

#[tokio::test]
async fn both_fixtures_coexist_and_are_independently_callable() {
    let registries = shared_registries();

    // Static fixture is always present.
    let static_tool = registries
        .tools
        .get("fixture-echo-static", &serde_json::Value::Null)
        .expect("static fixture must be registered");
    let static_result = static_tool
        .execute(serde_json::json!({ "source": "static" }))
        .await
        .expect("static tool execute should not fail");
    assert!(static_result.success);
    assert!(static_result.output.contains("static"));

    // Dynamic fixture is present when the cdylib is built.
    if !Path::new(&fixture_so_path()).exists() {
        eprintln!("skip: dynamic fixture cdylib not built — skipping dynamic half");
        return;
    }
    let dynamic_tool = registries
        .tools
        .get("fixture-echo-dynamic", &serde_json::Value::Null)
        .expect("dynamic fixture must be registered");
    let dynamic_result = dynamic_tool
        .execute(serde_json::json!({ "source": "dynamic" }))
        .await
        .expect("dynamic tool execute should not fail");
    assert!(dynamic_result.success);
    assert!(dynamic_result.output.contains("dynamic"));
}

// ── v2.5c: process-global accessor ───────────────────────────────────────────

#[test]
fn global_accessor_returns_initialized_registries() {
    let _ = shared_registries(); // ensure init
    let global = plugin_runtime::registries().expect("global must be initialized");
    assert!(global.tools.contains("fixture-echo-static"));
    assert!(plugin_runtime::is_initialized());
}

// ── v2.5c: runtime fallback semantics ────────────────────────────────────────
//
// The agent's tool executor (in `zeroclaw-runtime/src/agent/agent.rs`) consults
// the global `RegistrySet` only after the built-in tool list and the activated
// MCP toolset have failed. We can't easily spin up a full Agent here — that
// requires provider configuration, channels, etc. — but we can verify the
// underlying invariant: a tool name unknown to the host's static registries
// IS resolvable through the plugin registry, which is what makes the agent's
// `else if` branch fire.

#[test]
fn fallback_path_resolves_plugin_only_tool_name() {
    let registries = shared_registries();
    // Neither "fixture-echo-static" nor "fixture-echo-dynamic" is a built-in
    // tool name in this binary — only the plugins register them. If the
    // agent's lookup chain reaches the registry fallback, these are the names
    // it would resolve.
    assert!(
        registries.tools.contains("fixture-echo-static"),
        "registry must contain the static plugin-only tool name"
    );
    // Conversely, names not registered anywhere should not be present.
    assert!(
        !registries.tools.contains("definitely-not-a-real-tool"),
        "unrelated names must not appear"
    );
}
