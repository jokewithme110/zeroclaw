use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::memory_traits::{Memory, MemoryStrategy};
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_api::plugin::PluginRegistry as _;

use crate::agent::memory_loader::{DefaultMemoryLoader, MemoryLoader};

/// Resolve the effective memory strategy for a component.
///
/// If `memory_config.strategy` names a registered plugin strategy, instantiate
/// it from the process-global plugin registry. Otherwise fall back to the
/// built-in [`DefaultMemoryStrategy`]. Errors during plugin instantiation are
/// logged and treated as fallback, matching the best-effort native-plugin
/// loading policy.
///
/// `memory` and `workspace_dir` are passed through to `DefaultMemoryStrategy`
/// when no plugin strategy is selected (or when lookup fails). `limit` controls
/// the per-agent recall limit for the default strategy; plugin strategies read
/// their own configuration from the JSON config payload.
pub fn resolve_memory_strategy(
    memory_config: zeroclaw_config::schema::MemoryConfig,
    memory: Arc<dyn Memory>,
    workspace_dir: impl Into<PathBuf>,
    limit: usize,
) -> Arc<dyn MemoryStrategy> {
    let workspace_dir = workspace_dir.into();

    if let Some(name) = memory_config
        .strategy
        .as_deref()
        .filter(|n| !n.is_empty() && *n != "default")
    {
        if let Some(registries) = zeroclaw_api::plugin::runtime::registries() {
            if registries.memory_strategies.contains(name) {
                let cfg_json = match serde_json::to_value(&memory_config) {
                    Ok(v) => v,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Skip
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "strategy": name,
                                "error": e.to_string(),
                            })),
                            "failed to serialize memory config for plugin strategy; fallback to default"
                        );
                        return Arc::new(DefaultMemoryStrategy::with_config_and_limit(
                            memory,
                            memory_config,
                            workspace_dir,
                            limit,
                        ));
                    }
                };

                match registries.memory_strategies.get(name, &cfg_json) {
                    Ok(strategy) => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Load
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({ "strategy": name })),
                            "loaded plugin-provided memory strategy"
                        );
                        return Arc::from(strategy);
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Skip
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "strategy": name,
                                "error": e.to_string(),
                            })),
                            "failed to instantiate plugin memory strategy; fallback to default"
                        );
                    }
                }
            } else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "strategy": name })),
                    "memory.strategy names an unregistered plugin strategy; fallback to default"
                );
            }
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "strategy": name })),
                "plugin registries not initialized yet; memory.strategy ignored; fallback to default"
            );
        }
    }

    Arc::new(DefaultMemoryStrategy::with_config_and_limit(
        memory,
        memory_config,
        workspace_dir,
        limit,
    ))
}

/// Default memory strategy that delegates to existing implementations.
///
/// Phase 1: This is a thin wrapper. It does not duplicate logic;
/// it calls `DefaultMemoryLoader`, `consolidation::consolidate_turn`,
/// and `hygiene::run_if_due` directly, preserving current behavior
/// byte-for-byte.
pub struct DefaultMemoryStrategy {
    memory: Arc<dyn Memory>,
    limit: usize,
    min_relevance_score: f64,
    memory_config: zeroclaw_config::schema::MemoryConfig,
    workspace_dir: std::path::PathBuf,
}

impl DefaultMemoryStrategy {
    pub fn new(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        // #6722: rerank_enabled is declared on the config schema but the
        // retrieval-pipeline rerank stage was never landed (PR #4245 closed
        // unmerged).  Emit a one-time warning so operators who set these
        // fields know they currently have no effect.
        if memory_config.rerank_enabled {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "rerank_enabled": true,
                        "rerank_threshold": memory_config.rerank_threshold,
                    })),
                "memory.rerank_enabled is set but the rerank stage is not yet implemented; this setting currently has no effect"
            );
        }
        Self {
            memory,
            limit: 5,
            min_relevance_score: memory_config.min_relevance_score,
            memory_config,
            workspace_dir: workspace_dir.into(),
        }
    }

    /// Convenience constructor that takes the live `MemoryConfig` so
    /// `run_governance` uses the operator's actual settings (archive
    /// windows, hygiene toggle, etc.) rather than hardcoded defaults.
    pub fn with_config(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new(memory, memory_config, workspace_dir)
    }

    /// Build a strategy using the effective per-agent recall limit resolved by
    /// the caller while preserving the rest of the live memory configuration.
    pub fn with_config_and_limit(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
        limit: usize,
    ) -> Self {
        let mut strategy = Self::new(memory, memory_config, workspace_dir);
        strategy.limit = limit.max(1);
        strategy
    }
}

#[async_trait::async_trait]
impl MemoryStrategy for DefaultMemoryStrategy {
    async fn load_context(&self, query: &str, session_id: Option<&str>) -> anyhow::Result<String> {
        let loader = DefaultMemoryLoader::new(self.limit, self.min_relevance_score);
        loader
            .load_context(self.memory.as_ref(), query, session_id)
            .await
    }

    async fn consolidate_turn(
        &self,
        user_message: &str,
        assistant_response: &str,
        provider: &dyn ModelProvider,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<()> {
        zeroclaw_memory::consolidation::consolidate_turn(
            provider,
            model,
            temperature,
            self.memory.as_ref(),
            user_message,
            assistant_response,
        )
        .await
    }

    async fn run_governance(&self) -> anyhow::Result<()> {
        // Delegate to the existing hygiene routine.
        // Phase 1: `hygiene::run_if_due` returns `Result<()>`.
        // A structured report will be wired in a follow-up when hygiene
        // exposes per-action counters.
        zeroclaw_memory::hygiene::run_if_due(&self.memory_config, &self.workspace_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::MemoryConfig;

    fn none_memory() -> Arc<dyn Memory> {
        Arc::new(zeroclaw_memory::NoneMemory::new("none"))
    }

    #[test]
    fn resolves_to_default_when_strategy_unset() {
        // No `strategy` field set → built-in default, no panic.
        let cfg = MemoryConfig::default();
        let strategy = resolve_memory_strategy(cfg, none_memory(), std::path::PathBuf::new(), 5);
        // The returned object is usable; we can't downcast a trait object, but
        // reaching here without panicking is the contract under test.
        let _ = strategy;
    }

    #[test]
    fn resolves_to_default_when_strategy_is_literal_default() {
        let mut cfg = MemoryConfig::default();
        cfg.strategy = Some("default".to_string());
        let strategy = resolve_memory_strategy(cfg, none_memory(), std::path::PathBuf::new(), 5);
        let _ = strategy;
    }

    #[test]
    fn falls_back_to_default_for_unregistered_strategy() {
        // Names a plugin strategy that isn't registered. With no global
        // registry installed (or the name absent), the resolver must log a
        // warning and fall back to the default rather than panic.
        let mut cfg = MemoryConfig::default();
        cfg.strategy = Some("definitely-not-registered".to_string());
        let strategy = resolve_memory_strategy(cfg, none_memory(), std::path::PathBuf::new(), 5);
        let _ = strategy;
    }
}
