//! Generic plugin registry: factory registration, component retrieval, and the unified
//! [`RegistrySet`] that bundles all 8 component registries.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use crate::error::ZeroClawError;

// ── Factory type ───────────────────────────────────────────────────────────────

/// A thread-safe factory closure that produces boxed trait-object instances from a config value.
pub type Factory<T, C> = Arc<dyn Fn(&C) -> anyhow::Result<Box<T>> + Send + Sync>;

// ── PluginRegistry trait ───────────────────────────────────────────────────────

/// A registry that stores named factories and creates component instances on demand.
///
/// All implementations must be `Send + Sync` so the registry can be shared across threads.
pub trait PluginRegistry<T: ?Sized + 'static, C: 'static>: Send + Sync {
    /// Register a pre-wrapped [`Factory`].
    ///
    /// The name is lowercased before storage. Registering the same name twice overwrites the
    /// previous factory.
    fn register_factory(&self, name: &str, factory: Factory<T, C>);

    /// Retrieve a component instance by name using the given config.
    ///
    /// Name lookup is case-insensitive (internally, all names are stored lowercase).
    ///
    /// # Errors
    ///
    /// Returns [`ZeroClawError::ComponentNotFound`] (wrapped in `anyhow::Error`) when the name
    /// has no registered factory.
    fn get(&self, name: &str, config: &C) -> anyhow::Result<Box<T>>;

    /// Return all registered component names sorted alphabetically.
    fn list_registered(&self) -> Vec<String>;

    /// Return `true` if a component with the given name is registered (case-insensitive).
    fn contains(&self, name: &str) -> bool;

    /// Convenience wrapper: register a bare closure (auto-wraps it in [`Arc`]).
    ///
    /// Excluded from the trait-object vtable via `where Self: Sized`.
    fn register<F>(&self, name: impl Into<String>, factory: F)
    where
        F: Fn(&C) -> anyhow::Result<Box<T>> + Send + Sync + 'static,
        Self: Sized,
    {
        self.register_factory(&name.into(), Arc::new(factory));
    }
}

// ── HashMapRegistry ────────────────────────────────────────────────────────────

/// Concrete registry backed by a `HashMap` behind an `RwLock`.
///
/// Registration happens at startup (write lock); reads during runtime (read lock).
pub struct HashMapRegistry<T: ?Sized, C> {
    factories: RwLock<HashMap<String, Factory<T, C>>>,
}

impl<T: ?Sized, C> Default for HashMapRegistry<T, C> {
    fn default() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }
}

impl<T: ?Sized + 'static, C: 'static> PluginRegistry<T, C> for HashMapRegistry<T, C> {
    fn register_factory(&self, name: &str, factory: Factory<T, C>) {
        let key = name.to_lowercase();
        self.factories
            .write()
            .expect("plugin registry write lock poisoned")
            .insert(key, factory);
    }

    fn get(&self, name: &str, config: &C) -> anyhow::Result<Box<T>> {
        let key = name.to_lowercase();
        let guard = self
            .factories
            .read()
            .expect("plugin registry read lock poisoned");

        let factory = guard.get(&key).ok_or_else(|| {
            let mut names: Vec<&str> = guard.keys().map(String::as_str).collect();
            names.sort_unstable();
            ZeroClawError::ComponentNotFound {
                name: name.to_string(),
                available: names.join(", "),
            }
        })?;

        factory(config)
    }

    fn list_registered(&self) -> Vec<String> {
        let guard = self
            .factories
            .read()
            .expect("plugin registry read lock poisoned");
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        names
    }

    fn contains(&self, name: &str) -> bool {
        let key = name.to_lowercase();
        self.factories
            .read()
            .expect("plugin registry read lock poisoned")
            .contains_key(&key)
    }
}

// ── Config type aliases ────────────────────────────────────────────────────────

/// Config type for [`ModelProvider`](crate::model_provider::ModelProvider) factories.
pub type ProviderConfig = serde_json::Value;
/// Config type for [`Tool`](crate::tool::Tool) factories.
pub type ToolConfig = serde_json::Value;
/// Config type for [`Channel`](crate::channel::Channel) factories.
pub type ChannelConfig = serde_json::Value;
/// Config type for [`Memory`](crate::memory_traits::Memory) factories.
pub type MemoryConfig = serde_json::Value;
/// Config type for [`MemoryStrategy`](crate::memory_traits::MemoryStrategy) factories.
pub type MemoryStrategyConfig = serde_json::Value;
/// Config type for [`Observer`](crate::observability_traits::Observer) factories.
pub type ObserverConfig = serde_json::Value;
/// Config type for [`RuntimeAdapter`](crate::runtime_traits::RuntimeAdapter) factories.
pub type RuntimeConfig = serde_json::Value;
/// Config type for [`Peripheral`](crate::peripherals_traits::Peripheral) factories.
pub type PeripheralConfig = serde_json::Value;

// ── Registry type aliases ──────────────────────────────────────────────────────

/// Registry for [`ModelProvider`](crate::model_provider::ModelProvider) components.
pub type ProviderRegistry =
    HashMapRegistry<dyn crate::model_provider::ModelProvider, ProviderConfig>;
/// Registry for [`Tool`](crate::tool::Tool) components.
pub type ToolRegistry = HashMapRegistry<dyn crate::tool::Tool, ToolConfig>;
/// Registry for [`Channel`](crate::channel::Channel) components.
pub type ChannelRegistry = HashMapRegistry<dyn crate::channel::Channel, ChannelConfig>;
/// Registry for [`Memory`](crate::memory_traits::Memory) components.
pub type MemoryRegistry = HashMapRegistry<dyn crate::memory_traits::Memory, MemoryConfig>;
/// Registry for [`MemoryStrategy`](crate::memory_traits::MemoryStrategy) components.
pub type MemoryStrategyRegistry =
    HashMapRegistry<dyn crate::memory_traits::MemoryStrategy, MemoryStrategyConfig>;
/// Registry for [`Observer`](crate::observability_traits::Observer) components.
/// Note: `Observer` already has a `'static` supertrait; adding `+ 'static` here is redundant.
pub type ObserverRegistry =
    HashMapRegistry<dyn crate::observability_traits::Observer, ObserverConfig>;
/// Registry for [`RuntimeAdapter`](crate::runtime_traits::RuntimeAdapter) components.
pub type RuntimeRegistry =
    HashMapRegistry<dyn crate::runtime_traits::RuntimeAdapter, RuntimeConfig>;
/// Registry for [`Peripheral`](crate::peripherals_traits::Peripheral) components.
pub type PeripheralRegistry =
    HashMapRegistry<dyn crate::peripherals_traits::Peripheral, PeripheralConfig>;

// ── RegistrySet ───────────────────────────────────────────────────────────────

/// Unified bundle of all 8 component registries.
///
/// Create one `RegistrySet` per runtime instance and pass it to plugin init functions.
pub struct RegistrySet {
    pub providers: ProviderRegistry,
    pub tools: ToolRegistry,
    pub channels: ChannelRegistry,
    pub memory: MemoryRegistry,
    pub memory_strategies: MemoryStrategyRegistry,
    pub observers: ObserverRegistry,
    pub runtimes: RuntimeRegistry,
    pub peripherals: PeripheralRegistry,
}

impl RegistrySet {
    /// Create a new `RegistrySet` with all registries empty.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: HashMapRegistry::default(),
            tools: HashMapRegistry::default(),
            channels: HashMapRegistry::default(),
            memory: HashMapRegistry::default(),
            memory_strategies: HashMapRegistry::default(),
            observers: HashMapRegistry::default(),
            runtimes: HashMapRegistry::default(),
            peripherals: HashMapRegistry::default(),
        }
    }
}

impl Default for RegistrySet {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolResult};

    // ── Mock ──────────────────────────────────────────────────────────────────

    struct MockTool {
        name: String,
    }

    crate::mock_tool_attribution!(MockTool);

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock tool for tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: format!("executed {}", self.name),
                error: None,
            })
        }
    }

    fn make_registry() -> HashMapRegistry<dyn Tool, ToolConfig> {
        HashMapRegistry::default()
    }

    // ── T005-T008: Registration & retrieval ───────────────────────────────────

    #[test]
    fn test_register_and_get() {
        let reg = make_registry();
        reg.register("alpha", |_cfg| {
            Ok(Box::new(MockTool {
                name: "alpha".to_string(),
            }))
        });
        let config = serde_json::Value::Null;
        let result = reg.get("alpha", &config);
        assert!(result.is_ok(), "expected Ok, got error");
        assert_eq!(result.unwrap().name(), "alpha");
    }

    #[test]
    fn test_get_unknown_component() {
        let reg = make_registry();
        reg.register("beta", |_cfg| {
            Ok(Box::new(MockTool {
                name: "beta".to_string(),
            }))
        });
        let err = match reg.get("unknown", &serde_json::Value::Null) {
            Ok(_) => panic!("expected error for unknown component"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("unknown"),
            "error should mention the name: {msg}"
        );
        assert!(msg.contains("beta"), "error should list available: {msg}");
    }

    #[test]
    fn test_overwrite_existing() {
        let reg = make_registry();
        reg.register("gamma", |_cfg| {
            Ok(Box::new(MockTool {
                name: "first".to_string(),
            }))
        });
        reg.register("gamma", |_cfg| {
            Ok(Box::new(MockTool {
                name: "second".to_string(),
            }))
        });
        let tool = reg.get("gamma", &serde_json::Value::Null).unwrap();
        assert_eq!(tool.name(), "second", "second factory should win");
    }

    // ── T009: Thread safety ───────────────────────────────────────────────────

    #[test]
    fn test_thread_safety() {
        use std::{sync::Arc, thread};

        let reg = Arc::new(make_registry());
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let reg = Arc::clone(&reg);
                thread::spawn(move || {
                    let name = format!("tool-{i}");
                    reg.register(name.clone(), move |_cfg| {
                        Ok(Box::new(MockTool { name: name.clone() }))
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(reg.list_registered().len(), 10);
    }

    // ── T010: Query operations ────────────────────────────────────────────────

    #[test]
    fn test_list_registered() {
        let reg = make_registry();
        reg.register("zebra", |_cfg| {
            Ok(Box::new(MockTool {
                name: "zebra".to_string(),
            }))
        });
        reg.register("apple", |_cfg| {
            Ok(Box::new(MockTool {
                name: "apple".to_string(),
            }))
        });
        reg.register("mango", |_cfg| {
            Ok(Box::new(MockTool {
                name: "mango".to_string(),
            }))
        });
        let names = reg.list_registered();
        assert_eq!(names, vec!["apple", "mango", "zebra"]);
    }

    #[test]
    fn test_case_insensitive() {
        let reg = make_registry();
        reg.register("OpenAI", |_cfg| {
            Ok(Box::new(MockTool {
                name: "OpenAI".to_string(),
            }))
        });
        // stored as "openai"
        assert!(reg.contains("openai"));
        assert!(reg.contains("OPENAI"));
        assert!(reg.contains("OpenAI"));
    }

    // ── T014: RegistrySet ─────────────────────────────────────────────────────

    #[test]
    fn test_registry_set_new() {
        let rs = RegistrySet::new();
        assert!(rs.providers.list_registered().is_empty());
        assert!(rs.tools.list_registered().is_empty());
        assert!(rs.channels.list_registered().is_empty());
        assert!(rs.memory.list_registered().is_empty());
        assert!(rs.memory_strategies.list_registered().is_empty());
        assert!(rs.observers.list_registered().is_empty());
        assert!(rs.runtimes.list_registered().is_empty());
        assert!(rs.peripherals.list_registered().is_empty());
    }
}
