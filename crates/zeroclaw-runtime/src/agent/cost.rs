use crate::cost::CostTracker;
use crate::cost::types::{BudgetCheck, TokenUsage as CostTokenUsage};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use zeroclaw_config::schema::Config;

// ── Cost tracking via task-local ──

/// Per-provider pricing snapshot consumed by the cost tracker.
///
/// Outer key: model provider alias (e.g. `openrouter`, `anthropic`,
/// `azure-openai`). Inner key: user-defined model identifier, optionally
/// suffixed with `.input` / `.output` to encode pricing dimension. Values
/// are USD per 1M tokens.
pub type ModelProviderPricing = HashMap<String, HashMap<String, f64>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMapKeyMode {
    Alias,
    Type,
}

pub fn build_model_provider_pricing(
    config: &Config,
    key_mode: PricingMapKeyMode,
) -> ModelProviderPricing {
    let mut pricing: ModelProviderPricing = HashMap::new();

    match key_mode {
        PricingMapKeyMode::Alias => {
            for (type_k, alias_k, profile) in config.providers.models.iter_entries() {
                let mut slot = default_and_rate_pricing_for_provider(config, type_k);
                for (key, value) in &profile.pricing {
                    slot.insert(key.clone(), *value);
                }
                if !slot.is_empty() {
                    pricing.insert(format!("{type_k}.{alias_k}"), slot);
                }
            }
        }
        PricingMapKeyMode::Type => {
            for (type_k, _alias_k, profile) in config.providers.models.iter_entries() {
                let slot = pricing
                    .entry(type_k.to_string())
                    .or_insert_with(|| default_and_rate_pricing_for_provider(config, type_k));
                for (key, value) in &profile.pricing {
                    slot.insert(key.clone(), *value);
                }
            }
        }
    }

    pricing.retain(|_, slot| !slot.is_empty());
    pricing
}

fn default_and_rate_pricing_for_provider(
    config: &Config,
    provider_type: &str,
) -> HashMap<String, f64> {
    let mut slot = HashMap::new();

    for (catalog_key, model_pricing) in &config.cost.prices {
        let Some((catalog_provider_type, model_id)) = catalog_key.split_once('/') else {
            continue;
        };
        if catalog_provider_type != provider_type {
            continue;
        }
        slot.insert(format!("{catalog_key}.input"), model_pricing.input);
        slot.insert(format!("{catalog_key}.output"), model_pricing.output);
        slot.insert(format!("{model_id}.input"), model_pricing.input);
        slot.insert(format!("{model_id}.output"), model_pricing.output);
        if let Some(cached) = model_pricing.cached_input {
            slot.insert(format!("{catalog_key}.cached_input"), cached);
            slot.insert(format!("{model_id}.cached_input"), cached);
        }
    }

    for (rate_provider_type, model_id, rates) in config.cost.rates.providers.models.iter_entries() {
        if rate_provider_type != provider_type {
            continue;
        }
        if let Some(input) = rates.input_per_mtok {
            slot.insert(format!("{model_id}.input"), input);
        }
        if let Some(output) = rates.output_per_mtok {
            slot.insert(format!("{model_id}.output"), output);
        }
        if let Some(cached) = rates.cached_input_per_mtok {
            slot.insert(format!("{model_id}.cached_input"), cached);
        }
    }

    slot
}

/// Per-scope token/cost accumulator. Records pushed by
/// `record_tool_loop_cost_usage` alongside the shared `CostTracker` so the
/// wrapping code can read out the total for *this* call after the scope
/// exits, without racing concurrent requests sharing the same tracker.
#[derive(Default, Clone, Copy, Debug)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl TurnUsage {
    /// True when no tokens were billed and no cost was recorded. Used to
    /// skip emitting empty `AgentEnd` annotations when the turn produced
    /// no observable usage (e.g. a turn that returned early on error).
    pub fn is_zero(self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0 && self.cost_usd == 0.0
    }
}

/// Context for cost tracking within the tool call loop.
/// Scoped via `tokio::task_local!` at call sites (channels, gateway).
#[derive(Clone)]
pub struct ToolLoopCostTrackingContext {
    pub tracker: Arc<CostTracker>,
    pub model_provider_pricing: Arc<ModelProviderPricing>,
    pub turn_usage: Arc<Mutex<TurnUsage>>,
    /// Alias of the agent driving this turn. Stamped onto persisted
    /// `CostRecord`s so `/api/cost?agent=<alias>` can attribute spend.
    pub agent_alias: Option<String>,
}

impl ToolLoopCostTrackingContext {
    pub fn new(
        tracker: Arc<CostTracker>,
        model_provider_pricing: Arc<ModelProviderPricing>,
    ) -> Self {
        Self {
            tracker,
            model_provider_pricing,
            turn_usage: Arc::new(Mutex::new(TurnUsage::default())),
            agent_alias: None,
        }
    }

    /// Attach an agent alias to this context so subsequent
    /// `record_tool_loop_cost_usage` calls stamp records with it.
    #[must_use]
    pub fn with_agent_alias(mut self, agent_alias: impl Into<String>) -> Self {
        self.agent_alias = Some(agent_alias.into());
        self
    }

    /// Snapshot the per-scope usage. Wrapping code calls this after the
    /// scoped future completes to populate observer-event annotations.
    pub fn snapshot_turn_usage(&self) -> TurnUsage {
        *self.turn_usage.lock()
    }
}

tokio::task_local! {
    pub static TOOL_LOOP_COST_TRACKING_CONTEXT: Option<ToolLoopCostTrackingContext>;
}

/// Resolve `(input, output, cached_input)` per-1M-token rates for a given
/// model on a model provider's pricing map. Lookup order:
///
/// 1. Dimension-specific keys: `{model}.input` / `{model}.output` /
///    `{model}.cached_input`.
/// 2. Bare model key as a flat fallback applied to whichever dimension
///    didn't match in step 1.
/// 3. The model alias path's last segment (`.../suffix`) tried under the
///    same rules.
///
/// Returns `(0.0, 0.0, 0.0)` if no entry matches; the caller logs a
/// one-shot warn in that case. A zero `cached_input` rate means "no
/// discount" — the per-token caller bills the cached subset at the
/// standard input rate.
fn resolve_rates(pricing: &HashMap<String, f64>, model: &str) -> (f64, f64, f64) {
    let try_lookup = |key: &str| -> Option<(Option<f64>, Option<f64>, Option<f64>)> {
        let input = pricing.get(&format!("{key}.input")).copied();
        let output = pricing.get(&format!("{key}.output")).copied();
        let cached = pricing.get(&format!("{key}.cached_input")).copied();
        let flat = pricing.get(key).copied();
        if input.is_none() && output.is_none() && cached.is_none() && flat.is_none() {
            None
        } else {
            Some((input.or(flat), output.or(flat), cached))
        }
    };

    if let Some((input, output, cached)) = try_lookup(model) {
        return (
            input.unwrap_or(0.0),
            output.unwrap_or(0.0),
            cached.unwrap_or(0.0),
        );
    }
    if let Some((_, suffix)) = model.rsplit_once('/')
        && let Some((input, output, cached)) = try_lookup(suffix)
    {
        return (
            input.unwrap_or(0.0),
            output.unwrap_or(0.0),
            cached.unwrap_or(0.0),
        );
    }
    (0.0, 0.0, 0.0)
}

/// Resolve the per-model pricing map for a provider reference.
///
/// `model_provider_name` always arrives as the composite `<type>.<alias>`
/// (see `agent_provider_composite`), but the outer pricing map may be keyed
/// either way depending on which builder populated it: the CLI / cron / web
/// agent loop keys by the composite alias, while the channel orchestrator keys
/// by the bare provider `<type>` (rates are per provider type, not per alias).
/// Try the composite verbatim first, then fall back to the bare type prefix so
/// cost tracking resolves regardless of the builder — and so the type-keyed
/// `cost.rates` sheet is honored on the alias paths too.
fn provider_pricing<'a>(
    map: &'a ModelProviderPricing,
    model_provider_name: &str,
) -> Option<&'a HashMap<String, f64>> {
    map.get(model_provider_name).or_else(|| {
        model_provider_name
            .split_once('.')
            .and_then(|(provider_type, _alias)| map.get(provider_type))
    })
}

/// Record token usage from an LLM response via the task-local cost tracker.
/// Returns `(total_tokens, cost_usd)` on success, `None` when not scoped or no usage.
pub fn record_tool_loop_cost_usage(
    model_provider_name: &str,
    model: &str,
    usage: &zeroclaw_providers::traits::TokenUsage,
) -> Option<(u64, f64)> {
    let input_tokens = usage.input_tokens.unwrap_or(0);
    let output_tokens = usage.output_tokens.unwrap_or(0);
    let cached_input_tokens = usage.cached_input_tokens.unwrap_or(0);
    let total_tokens = input_tokens.saturating_add(output_tokens);
    if total_tokens == 0 {
        return None;
    }

    let ctx = TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()?;
    let pricing = provider_pricing(&ctx.model_provider_pricing, model_provider_name);
    let (input_rate, output_rate, cached_rate) = pricing
        .map(|map| resolve_rates(map, model))
        .unwrap_or((0.0, 0.0, 0.0));

    let cost_usage = CostTokenUsage::new_with_cache(
        model,
        input_tokens,
        cached_input_tokens,
        output_tokens,
        input_rate,
        cached_rate,
        output_rate,
    );

    // Promote first sighting of (model_provider, model) without pricing to a WARN
    // so operators notice the silent zero-cost record before they need to
    // grep DEBUG logs. Subsequent sightings stay at DEBUG so the warn
    // stream doesn't get spammy. Missing pricing means either the
    // model_provider has no pricing map at all, or the map exists but
    // produced zero rates for this model.
    if pricing.is_none() || (input_rate == 0.0 && output_rate == 0.0) {
        warn_once_missing_pricing(model_provider_name, model);
    }

    if let Err(error) = ctx
        .tracker
        .record_usage_with_agent(cost_usage.clone(), ctx.agent_alias.as_deref())
    {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": model_provider_name, "model": model, "error": format!("{}", error)})), "Failed to record cost tracking usage: ");
    } else {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "model": model,
                    "input_tokens": cost_usage.input_tokens,
                    "cached_input_tokens": cost_usage.cached_input_tokens,
                    "billable_input_tokens": cost_usage.billable_input_tokens,
                    "output_tokens": cost_usage.output_tokens,
                    "total_tokens": cost_usage.total_tokens,
                    "cost_usd": cost_usage.cost_usd,
                })),
            "cost usage recorded"
        );
    }

    {
        let mut usage = ctx.turn_usage.lock();
        usage.input_tokens = usage.input_tokens.saturating_add(input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(output_tokens);
        usage.cost_usd += cost_usage.cost_usd;
    }

    Some((cost_usage.total_tokens, cost_usage.cost_usd))
}

/// Insert `(model_provider, model)` into `seen`. Returns `true` on first sighting,
/// `false` thereafter. Split out from `warn_once_missing_pricing` so the
/// dedup contract can be unit-tested with a caller-owned set instead of the
/// process-static one.
fn missing_pricing_first_sighting(
    seen: &Mutex<HashSet<(String, String)>>,
    model_provider: &str,
    model: &str,
) -> bool {
    seen.lock()
        .insert((model_provider.to_string(), model.to_string()))
}

/// First-time WARN, subsequent DEBUG, per `(model_provider, model)` pair.
///
/// The default pricing catalog has no entries for most non-OpenAI/Anthropic/
/// Google models. Operators only realize their cost-tracking surface is
/// reporting zero when they happen to enable DEBUG logging — a pure-DEBUG
/// signal is too quiet for "your cost enforcement is silently inert" to
/// register. Promote the first sighting per-pair to WARN with a config-path
/// pointer; all subsequent same-pair occurrences stay at DEBUG so the warn
/// stream doesn't get spammy.
fn warn_once_missing_pricing(model_provider: &str, model: &str) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if missing_pricing_first_sighting(seen, model_provider, model) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"model_provider": model_provider, "model": model})
                ),
            "Cost tracking: no pricing entry found for {model_provider}/{model} — \
             token usage will be recorded with zero cost and budget enforcement \
             is inert for this model. Add a `pricing` table to the model provider \
             entry in config.toml (under `[providers.models.\"{model_provider}\"]`) \
             with `\"{model}.input\"` and `\"{model}.output\"` keys (USD per 1M tokens). \
             This warning fires once per (model_provider, model) pair per process."
        );
    } else {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"model_provider": model_provider, "model": model})
            ),
            "Cost tracking recorded token usage with zero pricing (no pricing entry found)"
        );
    }
}

/// Check budget before an LLM call. Returns `None` when no cost tracking
/// context is scoped (tests, delegate, CLI without cost config).
pub fn check_tool_loop_budget() -> Option<BudgetCheck> {
    TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map(|ctx| {
            ctx.tracker
                .check_budget(0.0)
                .unwrap_or(BudgetCheck::Allowed)
        })
}

/// Snapshot the currently scoped tool-loop cost usage, if any.
///
/// Returns `None` when no task-local cost tracking context is active or when
/// the scoped accumulator has not recorded any usage yet.
pub fn snapshot_scoped_turn_usage() -> Option<TurnUsage> {
    TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map(|ctx| ctx.snapshot_turn_usage())
        .filter(|usage| usage.input_tokens > 0 || usage.output_tokens > 0 || usage.cost_usd != 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{
        Config, DeepseekModelProviderConfig, ModelProviderConfig, OpenAIModelProviderConfig,
    };

    fn fresh_seen() -> Mutex<HashSet<(String, String)>> {
        Mutex::new(HashSet::new())
    }

    #[test]
    fn first_sighting_returns_true() {
        let seen = fresh_seen();
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "first observation of a (model_provider, model) pair must report first-sighting"
        );
    }

    #[test]
    fn second_sighting_same_pair_returns_false() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M2.7"
        ));
        assert!(
            !missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "second sighting of the same pair must NOT re-fire WARN"
        );
    }

    #[test]
    fn different_models_under_same_provider_are_independent() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M2.7"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M3.0"),
            "different model under same model_provider is a distinct pair"
        );
    }

    #[test]
    fn provider_pricing_resolves_composite_and_bare_type_keys() {
        let mut model_rates: HashMap<String, f64> = HashMap::new();
        model_rates.insert("glm-5.1.input".to_string(), 1.4);
        model_rates.insert("glm-5.1.output".to_string(), 4.4);

        // CLI / agent-loop builder keys by the composite `<type>.<alias>`.
        let mut composite_keyed: ModelProviderPricing = HashMap::new();
        composite_keyed.insert("glm.default".to_string(), model_rates.clone());
        assert!(
            provider_pricing(&composite_keyed, "glm.default").is_some(),
            "composite-keyed map must resolve via the verbatim composite lookup"
        );

        // Channel orchestrator builder keys by the bare provider `<type>`, yet
        // the lookup still arrives as the composite alias — must fall back.
        let mut type_keyed: ModelProviderPricing = HashMap::new();
        type_keyed.insert("glm".to_string(), model_rates.clone());
        assert!(
            provider_pricing(&type_keyed, "glm.default").is_some(),
            "type-keyed map must resolve the composite alias via the bare-type fallback"
        );

        // An unrelated provider must not accidentally match.
        assert!(
            provider_pricing(&type_keyed, "openai.default").is_none(),
            "fallback must not resolve a provider type absent from the map"
        );
    }

    #[test]
    fn different_providers_for_same_model_are_independent() {
        // Same model name served by two different model_providers — operator may
        // configure them at different rates, so the warn must fire for each.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "openrouter",
            "anthropic/claude-sonnet-4-5"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "anthropic", "anthropic/claude-sonnet-4-5"),
            "different model_provider for the same model is a distinct pair"
        );
    }

    #[test]
    fn empty_strings_dedup_independently() {
        // Defensive: empty model_provider or model shouldn't collide with each other.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(&seen, "", "model"));
        assert!(missing_pricing_first_sighting(&seen, "model_provider", ""));
        assert!(missing_pricing_first_sighting(&seen, "", ""));
        assert!(!missing_pricing_first_sighting(&seen, "", ""));
    }

    fn pricing_with_cache(
        model: &str,
        input: f64,
        cached_input: f64,
        output: f64,
    ) -> HashMap<String, f64> {
        let mut map = HashMap::new();
        map.insert(format!("{model}.input"), input);
        map.insert(format!("{model}.cached_input"), cached_input);
        map.insert(format!("{model}.output"), output);
        map
    }

    #[test]
    fn build_model_provider_pricing_merges_default_catalog_and_alias_overrides() {
        let mut config = Config::default();
        config.providers.models.deepseek.insert(
            "default".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("deepseek-v4-flash".into()),
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.77)]),
                    ..Default::default()
                },
            },
        );
        config.providers.models.openai.insert(
            "default".to_string(),
            OpenAIModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("gpt-4o".into()),
                    ..Default::default()
                },
            },
        );

        let alias_map = build_model_provider_pricing(&config, PricingMapKeyMode::Alias);
        let deepseek = alias_map
            .get("deepseek.default")
            .expect("deepseek alias pricing");
        assert_eq!(deepseek.get("deepseek-v4-flash.input").copied(), Some(0.14));
        assert_eq!(
            deepseek.get("deepseek-v4-flash.cached_input").copied(),
            Some(0.0028)
        );
        assert_eq!(
            deepseek.get("deepseek-v4-flash.output").copied(),
            Some(0.77),
            "alias-level pricing must override default catalog"
        );

        let openai = alias_map
            .get("openai.default")
            .expect("openai alias pricing");
        assert_eq!(openai.get("gpt-4o.input").copied(), Some(5.0));
        assert_eq!(openai.get("gpt-4o.output").copied(), Some(15.0));
    }

    #[test]
    fn record_tool_loop_cost_usage_applies_cached_input_pricing() {
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                workspace.path(),
            )
            .unwrap(),
        );
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(
                "deepseek".to_string(),
                pricing_with_cache("deepseek-chat", 0.27, 0.027, 1.10),
            )])),
        );
        let usage = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(5_000),
            output_tokens: Some(200),
            cached_input_tokens: Some(4_000),
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (total_tokens, cost_usd) = runtime
            .block_on(TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(ctx), async {
                record_tool_loop_cost_usage("deepseek", "deepseek-chat", &usage)
            }))
            .expect("cost usage");

        let expected = (1_000.0 * 0.27 / 1_000_000.0)
            + (4_000.0 * 0.027 / 1_000_000.0)
            + (200.0 * 1.10 / 1_000_000.0);
        assert_eq!(total_tokens, 5_200);
        assert!((cost_usd - expected).abs() < 1e-12);

        let stored = std::fs::read_to_string(workspace.path().join("state").join("costs.jsonl"))
            .expect("costs.jsonl should be written");
        let record: zeroclaw_config::cost::types::CostRecord =
            serde_json::from_str(stored.lines().next().expect("one record")).unwrap();
        assert_eq!(record.usage.cached_input_tokens, 4_000);
        assert_eq!(record.usage.billable_input_tokens, 1_000);
    }

    #[tokio::test]
    async fn snapshot_scoped_turn_usage_returns_accumulated_usage() {
        let workspace = tempfile::TempDir::new().unwrap();
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::new(
                CostTracker::new(
                    zeroclaw_config::schema::CostConfig::default(),
                    workspace.path(),
                )
                .unwrap(),
            ),
            Arc::new(HashMap::new()),
        );
        {
            let mut usage = ctx.turn_usage.lock();
            usage.input_tokens = 10;
            usage.output_tokens = 5;
            usage.cost_usd = 0.25;
        }

        let snapshot = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async { snapshot_scoped_turn_usage() })
            .await;

        let usage = snapshot.expect("usage should be available");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert!((usage.cost_usd - 0.25).abs() < f64::EPSILON);
    }
}
