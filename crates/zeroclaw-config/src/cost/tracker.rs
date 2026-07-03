use super::types::{
    AgentCostStats, AggregateStats, BudgetCheck, CostAggregates, CostRecord, CostSummary,
    ModelStats, TokenPeriod, TokenStats, TokenSummary, TokenUsage, UsagePeriod,
};
use crate::schema::CostConfig;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use chrono_tz::Tz;
use parking_lot::{Mutex, MutexGuard};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

/// Cost tracker for API usage monitoring and budget enforcement.
pub struct CostTracker {
    config: CostConfig,
    storage: Arc<Mutex<CostStorage>>,
    session_id: String,
    /// Per-daemon-lifetime aggregates keyed by `Option<agent_alias>`,
    /// replacing the unbounded per-turn `Vec<CostRecord>`.
    session_totals: Arc<Mutex<HashMap<Option<String>, AgentTotals>>>,
}

#[derive(Default, Clone, Copy)]
struct AgentTotals {
    cost_usd: f64,
    total_tokens: u64,
    request_count: u64,
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new(config: CostConfig, workspace_dir: &Path) -> Result<Self> {
        let storage_path = resolve_storage_path(workspace_dir)?;
        let storage = CostStorage::new(&storage_path, &config).with_context(|| {
            format!(
                "Failed to open cost storage at {}",
                storage_path.display().to_string()
            )
        })?;

        Ok(Self {
            config,
            storage: Arc::new(Mutex::new(storage)),
            session_id: uuid::Uuid::new_v4().to_string(),
            session_totals: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn lock_storage(&self) -> MutexGuard<'_, CostStorage> {
        self.storage.lock()
    }

    fn lock_session_totals(&self) -> MutexGuard<'_, HashMap<Option<String>, AgentTotals>> {
        self.session_totals.lock()
    }

    /// Check if a request is within budget.
    pub fn check_budget(&self, estimated_cost_usd: f64) -> Result<BudgetCheck> {
        if !self.config.enabled {
            return Ok(BudgetCheck::Allowed);
        }

        if !estimated_cost_usd.is_finite() || estimated_cost_usd < 0.0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"estimated_cost_usd": estimated_cost_usd})),
                "cost budget check rejected: estimated cost is not finite or is negative"
            );
            anyhow::bail!("Estimated cost must be a finite, non-negative value");
        }

        let mut storage = self.lock_storage();
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs()?;

        // Check daily limit
        let projected_daily = daily_cost + estimated_cost_usd;
        if projected_daily > self.config.daily_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: daily_cost,
                limit_usd: self.config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        // Check monthly limit
        let projected_monthly = monthly_cost + estimated_cost_usd;
        if projected_monthly > self.config.monthly_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: monthly_cost,
                limit_usd: self.config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        // Check warning thresholds
        let warn_threshold = f64::from(self.config.warn_at_percent.min(100)) / 100.0;
        let daily_warn_threshold = self.config.daily_limit_usd * warn_threshold;
        let monthly_warn_threshold = self.config.monthly_limit_usd * warn_threshold;

        if projected_daily >= daily_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: daily_cost,
                limit_usd: self.config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        if projected_monthly >= monthly_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: monthly_cost,
                limit_usd: self.config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        Ok(BudgetCheck::Allowed)
    }

    /// Record a usage event without per-agent attribution.
    pub fn record_usage(&self, usage: TokenUsage) -> Result<()> {
        self.record_usage_with_agent(usage, None)
    }

    /// Record a usage event attributed to a specific agent alias. When
    /// `[cost].track_per_agent` is false the alias is dropped before
    /// persistence.
    pub fn record_usage_with_agent(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        if !usage.cost_usd.is_finite() || usage.cost_usd < 0.0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"cost_usd": usage.cost_usd})),
                "token usage record rejected: cost is not finite or is negative"
            );
            anyhow::bail!("Token usage cost must be a finite, non-negative value");
        }

        let effective_alias = if self.config.track_per_agent {
            agent_alias.map(str::to_string)
        } else {
            None
        };
        let cost_usd = usage.cost_usd;
        let total_tokens = usage.total_tokens;
        let record = CostRecord::with_agent(&self.session_id, effective_alias.clone(), usage);

        {
            let mut storage = self.lock_storage();
            storage.add_record(record)?;
        }

        {
            let mut totals = self.lock_session_totals();
            let entry = totals.entry(effective_alias).or_default();
            entry.cost_usd += cost_usd;
            entry.total_tokens += total_tokens;
            entry.request_count += 1;
        }

        Ok(())
    }

    /// Get the current cost summary. When `[cost].track_per_agent` is
    /// enabled, the response includes a `by_agent` rollup over today's
    /// records.
    pub fn get_summary(&self) -> Result<CostSummary> {
        self.get_summary_filtered(None)
    }

    /// Filter persisted records by `[from, to)` (either side `None` is
    /// unbounded) and roll up by_model / by_agent / window totals.
    /// Bounds come from the caller (the dashboard computes them in the
    /// operator's local timezone); the tracker doesn't decide what
    /// "today" means.
    pub fn get_summary_in_bounds(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, records) = {
            let mut storage = self.lock_storage();
            let (d, m) = storage.get_aggregated_costs()?;
            let recs = storage.records_in_bounds(from, to)?;
            (d, m, recs)
        };
        let total_cost: f64 = records.iter().map(|r| r.usage.cost_usd).sum();
        let total_tokens: u64 = records.iter().map(|r| r.usage.total_tokens).sum();
        let request_count = records.len();
        let by_model = build_model_stats(records.iter());
        let by_agent = if self.config.track_per_agent {
            build_agent_stats(&records)
        } else {
            HashMap::new()
        };
        Ok(CostSummary {
            session_cost_usd: total_cost,
            daily_cost_usd: daily_cost,
            monthly_cost_usd: monthly_cost,
            total_tokens,
            request_count,
            by_model,
            by_agent,
        })
    }

    /// Get the current cost summary scoped to a single agent alias. The
    /// session/day/month figures and `by_model` are filtered to records
    /// attributed to that alias; `by_agent` is left empty since the
    /// caller already chose the dimension.
    pub fn get_summary_for_agent(&self, agent_alias: &str) -> Result<CostSummary> {
        self.get_summary_filtered(Some(agent_alias))
    }

    fn get_summary_filtered(&self, agent_filter: Option<&str>) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, daily_records) = {
            let mut storage = self.lock_storage();
            let (d, m) = storage.get_aggregated_costs()?;
            // Always pull daily_records: per-model and per-agent rollups
            // both want today's slice. The optional-skip optimisation tied
            // to `track_per_agent` made the by-model rollup session-scoped,
            // which surprised operators after a daemon restart and clashes
            // with the daily totals in the same response.
            (d, m, storage.daily_records()?)
        };

        let (session_cost, total_tokens, request_count) = {
            let totals = self.lock_session_totals();
            totals
                .iter()
                .filter(|(alias, _)| match agent_filter {
                    Some(want) => alias.as_deref() == Some(want),
                    None => true,
                })
                .fold((0.0_f64, 0_u64, 0_usize), |(c, t, r), (_, v)| {
                    (
                        c + v.cost_usd,
                        t + v.total_tokens,
                        r + v.request_count as usize,
                    )
                })
        };

        let matches_agent = |record: &CostRecord| match agent_filter {
            Some(alias) => record.agent_alias.as_deref() == Some(alias),
            None => true,
        };

        // Daily-scoped per-model rollup. Filter by agent when scoped.
        let model_records: Vec<&CostRecord> =
            daily_records.iter().filter(|r| matches_agent(r)).collect();
        let by_model = build_model_stats(model_records.iter().copied());

        let (daily_total, monthly_total, by_agent) = if let Some(alias) = agent_filter {
            // Per-agent view: re-aggregate day/month from persisted records.
            let mut daily_total = 0.0;
            let mut monthly_total = 0.0;
            let storage = self.lock_storage();
            let timezone = storage.timezone;
            let now = Utc::now().with_timezone(&timezone);
            let today = now.date_naive();
            for record in &daily_records {
                if record.agent_alias.as_deref() != Some(alias) {
                    continue;
                }
                let ts = record.usage.timestamp.with_timezone(&timezone);
                if ts.date_naive() == today {
                    daily_total += record.usage.cost_usd;
                }
                if ts.year() == now.year() && ts.month() == now.month() {
                    monthly_total += record.usage.cost_usd;
                }
            }
            (daily_total, monthly_total, HashMap::new())
        } else if self.config.track_per_agent {
            let by_agent = build_agent_stats(&daily_records);
            (daily_cost, monthly_cost, by_agent)
        } else {
            (daily_cost, monthly_cost, HashMap::new())
        };

        Ok(CostSummary {
            session_cost_usd: session_cost,
            daily_cost_usd: daily_total,
            monthly_cost_usd: monthly_total,
            total_tokens,
            request_count,
            by_model,
            by_agent,
        })
    }

    /// Get the daily cost for a specific date.
    pub fn get_daily_cost(&self, date: NaiveDate) -> Result<f64> {
        let storage = self.lock_storage();
        storage.get_cost_for_date(date)
    }

    /// Get the monthly cost for a specific month.
    pub fn get_monthly_cost(&self, year: i32, month: u32) -> Result<f64> {
        let storage = self.lock_storage();
        storage.get_cost_for_month(year, month)
    }

    /// Get the token summary for a specific date.
    pub fn get_token_summary_day(
        &self,
        date: chrono::NaiveDate,
        model: Option<&str>,
    ) -> Result<TokenSummary> {
        let storage = self.lock_storage();
        let tz = storage.timezone_name().to_string();
        let (models, totals) = storage.get_token_summary_for_date(date, model)?;
        Ok(TokenSummary {
            period: TokenPeriod::Day,
            date: Some(date),
            month: None,
            model: model.map(ToOwned::to_owned),
            tz,
            models,
            totals,
        })
    }

    /// Get the token summary for a specific month.
    pub fn get_token_summary_month(
        &self,
        year: i32,
        month: u32,
        model: Option<&str>,
    ) -> Result<TokenSummary> {
        let storage = self.lock_storage();
        let tz = storage.timezone_name().to_string();
        let (models, totals) = storage.get_token_summary_for_month(year, month, model)?;
        Ok(TokenSummary {
            period: TokenPeriod::Month,
            date: None,
            month: Some(format!("{year:04}-{month:02}")),
            model: model.map(ToOwned::to_owned),
            tz,
            models,
            totals,
        })
    }
}

// ── Process-global singleton ────────────────────────────────────────
// Both the gateway and the channels supervisor share a single CostTracker
// so that budget enforcement is consistent across all paths.

static GLOBAL_COST_TRACKER: OnceLock<Option<Arc<CostTracker>>> = OnceLock::new();

impl CostTracker {
    /// Return the process-global `CostTracker`, creating it on first call.
    /// Subsequent calls (from gateway or channels, whichever starts second)
    /// receive the same `Arc`.  Returns `None` when cost tracking is disabled
    /// or initialisation fails.
    pub fn get_or_init_global(config: CostConfig, workspace_dir: &Path) -> Option<Arc<Self>> {
        GLOBAL_COST_TRACKER
            .get_or_init(|| {
                if !config.enabled {
                    return None;
                }
                match Self::new(config, workspace_dir) {
                    Ok(ct) => Some(Arc::new(ct)),
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Failed to initialize global cost tracker"
                        );
                        None
                    }
                }
            })
            .clone()
    }
}

fn resolve_storage_path(workspace_dir: &Path) -> Result<PathBuf> {
    let storage_path = workspace_dir.join("state").join("costs.jsonl");
    let legacy_path = workspace_dir.join(".zeroclaw").join("costs.db");

    if !storage_path.exists() && legacy_path.exists() {
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }

        if let Err(error) = fs::rename(&legacy_path, &storage_path) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Failed to move legacy cost storage from {} to {}: {error}; falling back to copy",
                    legacy_path.display().to_string(),
                    storage_path.display().to_string()
                )
            );
            fs::copy(&legacy_path, &storage_path).with_context(|| {
                format!(
                    "Failed to copy legacy cost storage from {} to {}",
                    legacy_path.display().to_string(),
                    storage_path.display()
                )
            })?;
        }
    }

    Ok(storage_path)
}

fn build_model_stats<'a, I>(records: I) -> HashMap<String, ModelStats>
where
    I: IntoIterator<Item = &'a CostRecord>,
{
    let mut by_model: HashMap<String, ModelStats> = HashMap::new();

    for record in records {
        let entry = by_model
            .entry(record.usage.model.clone())
            .or_insert_with(|| ModelStats {
                model: record.usage.model.clone(),
                cost_usd: 0.0,
                total_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                billable_input_tokens: 0,
                request_count: 0,
            });

        entry.cost_usd += record.usage.cost_usd;
        entry.total_tokens += record.usage.total_tokens;
        entry.input_tokens += record.usage.input_tokens;
        entry.output_tokens += record.usage.output_tokens;
        entry.cached_input_tokens += record.usage.cached_input_tokens;
        entry.billable_input_tokens += record.usage.billable_input_tokens;
        entry.request_count += 1;
    }

    by_model
}

fn build_agent_stats(records: &[CostRecord]) -> HashMap<String, AgentCostStats> {
    let mut by_agent: HashMap<String, AgentCostStats> = HashMap::new();

    for record in records {
        let Some(alias) = record.agent_alias.as_deref() else {
            continue;
        };
        let entry = by_agent
            .entry(alias.to_string())
            .or_insert_with(|| AgentCostStats {
                agent_alias: alias.to_string(),
                cost_usd: 0.0,
                total_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                request_count: 0,
            });

        entry.cost_usd += record.usage.cost_usd;
        entry.total_tokens += record.usage.total_tokens;
        entry.input_tokens += record.usage.input_tokens;
        entry.output_tokens += record.usage.output_tokens;
        entry.cached_input_tokens += record.usage.cached_input_tokens;
        entry.request_count += 1;
    }

    by_agent
}

/// Persistent storage for cost records.
struct CostStorage {
    detail_path: PathBuf,
    aggregate_path: PathBuf,
    timezone_name: String,
    timezone: Tz,
    max_total_bytes: u64,
    max_detail_records: usize,
    retain_daily_days: usize,
    retain_monthly_months: usize,
    aggregates: CostAggregates,
}

impl CostStorage {
    /// Create or open cost storage.
    fn new(path: &Path, config: &CostConfig) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }
        let timezone_name = config.default_timezone.trim();
        let timezone = Tz::from_str(if timezone_name.is_empty() {
            "Asia/Shanghai"
        } else {
            timezone_name
        })
        .with_context(|| format!("Invalid cost default timezone: {}", config.default_timezone))?;
        let aggregate_path = path.with_file_name("cost_aggregates.json");
        let mut storage = Self {
            detail_path: path.to_path_buf(),
            aggregate_path,
            timezone_name: timezone.name().to_string(),
            timezone,
            max_total_bytes: config.storage.max_total_bytes,
            max_detail_records: config.storage.max_detail_records,
            retain_daily_days: config.storage.retain_daily_days,
            retain_monthly_months: config.storage.retain_monthly_months,
            aggregates: CostAggregates::default(),
        };
        storage.load_or_rebuild_aggregates()?;
        storage.prune_storage()?;
        storage.save_aggregates()?;

        Ok(storage)
    }

    fn timezone_name(&self) -> &str {
        &self.timezone_name
    }

    fn for_each_record<F>(&self, mut on_record: F) -> Result<()>
    where
        F: FnMut(CostRecord),
    {
        if !self.detail_path.exists() {
            return Ok(());
        }

        let file = File::open(&self.detail_path).with_context(|| {
            format!(
                "Failed to read cost storage from {}",
                self.detail_path.display().to_string()
            )
        })?;
        let reader = BufReader::new(file);

        for (line_number, line) in reader.lines().enumerate() {
            let raw_line = line.with_context(|| {
                format!(
                    "Failed to read line {} from cost storage {}",
                    line_number + 1,
                    self.detail_path.display()
                )
            })?;

            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<CostRecord>(trimmed) {
                Ok(record) => on_record(record),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "Skipping malformed cost record at {}:{}: {error}",
                            self.detail_path.display().to_string(),
                            line_number + 1
                        )
                    );
                }
            }
        }

        Ok(())
    }

    fn load_or_rebuild_aggregates(&mut self) -> Result<()> {
        if self.aggregate_path.exists() {
            let file = File::open(&self.aggregate_path).with_context(|| {
                format!(
                    "Failed to open cost aggregate storage at {}",
                    self.aggregate_path.display()
                )
            })?;
            match serde_json::from_reader::<_, CostAggregates>(file) {
                Ok(aggregates) if aggregates.tz == self.timezone_name => {
                    self.aggregates = aggregates;
                    return Ok(());
                }
                Ok(_) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Cost aggregate timezone changed; rebuilding aggregates for {}",
                            self.timezone_name
                        )
                    );
                }
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "Failed to parse cost aggregate storage at {}: {error}; rebuilding",
                            self.aggregate_path.display()
                        )
                    );
                }
            }
        }

        self.rebuild_aggregates_from_details()
    }

    fn rebuild_aggregates_from_details(&mut self) -> Result<()> {
        self.aggregates = CostAggregates {
            tz: self.timezone_name.clone(),
            daily: HashMap::new(),
            monthly: HashMap::new(),
        };

        let timezone = self.timezone;
        let records = self.load_records()?;
        for record in records {
            let (day_key, month_key) = period_keys_for_timezone(record.usage.timestamp, timezone);
            record_usage_in_aggregate(&mut self.aggregates.daily, &day_key, &record.usage);
            record_usage_in_aggregate(&mut self.aggregates.monthly, &month_key, &record.usage);
        }
        Ok(())
    }

    /// Add a new record.
    fn add_record(&mut self, record: CostRecord) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.detail_path)
            .with_context(|| {
                format!(
                    "Failed to open cost storage at {}",
                    self.detail_path.display().to_string()
                )
            })?;

        writeln!(file, "{}", serde_json::to_string(&record)?).with_context(|| {
            format!(
                "Failed to write cost record to {}",
                self.detail_path.display().to_string()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "Failed to sync cost storage at {}",
                self.detail_path.display().to_string()
            )
        })?;

        let (day_key, month_key) = period_keys_for_timezone(record.usage.timestamp, self.timezone);
        record_usage_in_aggregate(&mut self.aggregates.daily, &day_key, &record.usage);
        record_usage_in_aggregate(&mut self.aggregates.monthly, &month_key, &record.usage);
        self.prune_storage()?;
        self.save_aggregates()?;

        Ok(())
    }

    /// Get aggregated costs for current day and month.
    fn get_aggregated_costs(&mut self) -> Result<(f64, f64)> {
        let now = Utc::now().with_timezone(&self.timezone);
        let day_key = now.date_naive().to_string();
        let month_key = format!("{:04}-{:02}", now.year(), now.month());
        let daily_cost = self
            .aggregates
            .daily
            .get(&day_key)
            .and_then(|stats| stats.get(TOTAL_KEY))
            .map_or(0.0, |stats| stats.cost_usd);
        let monthly_cost = self
            .aggregates
            .monthly
            .get(&month_key)
            .and_then(|stats| stats.get(TOTAL_KEY))
            .map_or(0.0, |stats| stats.cost_usd);
        Ok((daily_cost, monthly_cost))
    }

    /// Snapshot every record whose timestamp falls within the current
    /// calendar month. Used to build per-agent rollups without folding a
    /// new aggregate table into the JSONL file.
    fn daily_records(&mut self) -> Result<Vec<CostRecord>> {
        let now = Utc::now().with_timezone(&self.timezone);
        let day_key = now.date_naive().to_string();
        let mut out = Vec::new();
        self.for_each_record(|record| {
            let (record_day_key, _) =
                period_keys_for_timezone(record.usage.timestamp, self.timezone);
            if record_day_key == day_key {
                out.push(record);
            }
        })?;
        Ok(out)
    }

    fn records_in_bounds(
        &mut self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<CostRecord>> {
        let mut out = Vec::new();
        self.for_each_record(|record| {
            let ts = record.usage.timestamp;
            if from.is_some_and(|f| ts < f) {
                return;
            }
            if to.is_some_and(|t| ts >= t) {
                return;
            }
            out.push(record);
        })?;
        Ok(out)
    }

    /// Get cost for a specific date.
    fn get_cost_for_date(&self, date: NaiveDate) -> Result<f64> {
        Ok(self
            .aggregates
            .daily
            .get(&date.to_string())
            .and_then(|stats| stats.get(TOTAL_KEY))
            .map_or(0.0, |stats| stats.cost_usd))
    }

    /// Get cost for a specific month.
    fn get_cost_for_month(&self, year: i32, month: u32) -> Result<f64> {
        let month_key = format!("{year:04}-{month:02}");
        Ok(self
            .aggregates
            .monthly
            .get(&month_key)
            .and_then(|stats| stats.get(TOTAL_KEY))
            .map_or(0.0, |stats| stats.cost_usd))
    }

    fn get_token_summary_for_date(
        &self,
        date: chrono::NaiveDate,
        model: Option<&str>,
    ) -> Result<(HashMap<String, TokenStats>, TokenStats)> {
        Ok(summary_from_period_map(
            self.aggregates.daily.get(&date.to_string()),
            model,
        ))
    }

    fn get_token_summary_for_month(
        &self,
        year: i32,
        month: u32,
        model: Option<&str>,
    ) -> Result<(HashMap<String, TokenStats>, TokenStats)> {
        let month_key = format!("{year:04}-{month:02}");
        Ok(summary_from_period_map(
            self.aggregates.monthly.get(&month_key),
            model,
        ))
    }

    fn save_aggregates(&self) -> Result<()> {
        if let Some(parent) = self.aggregate_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.aggregate_path)
            .with_context(|| {
                format!(
                    "Failed to open cost aggregate storage at {}",
                    self.aggregate_path.display()
                )
            })?;
        let payload = serde_json::to_string(&self.aggregates)?;
        file.write_all(payload.as_bytes()).with_context(|| {
            format!(
                "Failed to write cost aggregate storage at {}",
                self.aggregate_path.display()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "Failed to sync cost aggregate storage at {}",
                self.aggregate_path.display()
            )
        })?;
        Ok(())
    }

    fn prune_storage(&mut self) -> Result<()> {
        self.prune_detail_records()?;
        self.prune_aggregate_windows();

        while self.total_storage_bytes()? > self.max_total_bytes {
            if self.drop_oldest_detail_record()? {
                continue;
            }
            if self.drop_oldest_daily_period() {
                continue;
            }
            if self.drop_oldest_monthly_period() {
                continue;
            }
            break;
        }

        Ok(())
    }

    fn prune_detail_records(&self) -> Result<()> {
        let lines = self.read_detail_lines()?;
        if lines.len() <= self.max_detail_records {
            return Ok(());
        }
        let retained = lines[lines.len().saturating_sub(self.max_detail_records)..].to_vec();
        self.write_detail_lines(&retained)
    }

    fn prune_aggregate_windows(&mut self) {
        prune_period_map(&mut self.aggregates.daily, self.retain_daily_days);
        prune_period_map(&mut self.aggregates.monthly, self.retain_monthly_months);
    }

    fn total_storage_bytes(&self) -> Result<u64> {
        let detail_size = fs::metadata(&self.detail_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let aggregate_size = serde_json::to_vec(&self.aggregates)
            .map(|payload| payload.len() as u64)
            .with_context(|| {
                format!(
                    "Failed to size cost aggregate storage at {}",
                    self.aggregate_path.display()
                )
            })?;
        Ok(detail_size.saturating_add(aggregate_size))
    }

    fn drop_oldest_detail_record(&self) -> Result<bool> {
        let lines = self.read_detail_lines()?;
        if lines.is_empty() {
            return Ok(false);
        }
        let retained = lines[1..].to_vec();
        self.write_detail_lines(&retained)?;
        Ok(true)
    }

    fn drop_oldest_daily_period(&mut self) -> bool {
        drop_oldest_period(&mut self.aggregates.daily)
    }

    fn drop_oldest_monthly_period(&mut self) -> bool {
        drop_oldest_period(&mut self.aggregates.monthly)
    }

    fn read_detail_lines(&self) -> Result<Vec<String>> {
        if !self.detail_path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.detail_path).with_context(|| {
            format!(
                "Failed to read cost storage from {}",
                self.detail_path.display()
            )
        })?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            let raw_line = line.with_context(|| {
                format!(
                    "Failed to read cost storage from {}",
                    self.detail_path.display()
                )
            })?;
            if !raw_line.trim().is_empty() {
                lines.push(raw_line);
            }
        }
        Ok(lines)
    }

    fn load_records(&self) -> Result<Vec<CostRecord>> {
        let mut records = Vec::new();
        self.for_each_record(|record| records.push(record))?;
        Ok(records)
    }

    fn write_detail_lines(&self, lines: &[String]) -> Result<()> {
        if let Some(parent) = self.detail_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.detail_path)
            .with_context(|| {
                format!(
                    "Failed to open cost storage at {}",
                    self.detail_path.display()
                )
            })?;
        for line in lines {
            writeln!(file, "{line}").with_context(|| {
                format!(
                    "Failed to write cost storage at {}",
                    self.detail_path.display()
                )
            })?;
        }
        file.sync_all().with_context(|| {
            format!(
                "Failed to sync cost storage at {}",
                self.detail_path.display()
            )
        })?;
        Ok(())
    }
}

const TOTAL_KEY: &str = "_total";

fn period_keys_for_timezone(timestamp: chrono::DateTime<Utc>, timezone: Tz) -> (String, String) {
    let localized = timestamp.with_timezone(&timezone);
    (
        localized.date_naive().to_string(),
        format!("{:04}-{:02}", localized.year(), localized.month()),
    )
}

fn record_usage_in_aggregate(
    periods: &mut HashMap<String, HashMap<String, AggregateStats>>,
    period_key: &str,
    usage: &TokenUsage,
) {
    let entry = periods.entry(period_key.to_string()).or_default();
    entry
        .entry(TOTAL_KEY.to_string())
        .or_default()
        .record_usage(usage);
    entry
        .entry(usage.model.clone())
        .or_default()
        .record_usage(usage);
}

fn summary_from_period_map(
    period: Option<&HashMap<String, AggregateStats>>,
    model: Option<&str>,
) -> (HashMap<String, TokenStats>, TokenStats) {
    let mut models = HashMap::new();
    let mut totals = TokenStats::default();

    if let Some(period_map) = period {
        if let Some(model_name) = model {
            if let Some(stats) = period_map.get(model_name) {
                let converted = TokenStats::from(stats);
                totals = converted.clone();
                models.insert(model_name.to_string(), converted);
            }
        } else {
            for (name, stats) in period_map {
                if name == TOTAL_KEY {
                    totals = TokenStats::from(stats);
                    continue;
                }
                models.insert(name.clone(), TokenStats::from(stats));
            }
        }
    }

    (models, totals)
}

fn prune_period_map(periods: &mut HashMap<String, HashMap<String, AggregateStats>>, retain: usize) {
    while periods.len() > retain {
        if !drop_oldest_period(periods) {
            break;
        }
    }
}

fn drop_oldest_period(periods: &mut HashMap<String, HashMap<String, AggregateStats>>) -> bool {
    let Some(oldest_key) = periods.keys().min().cloned() else {
        return false;
    };
    periods.remove(&oldest_key);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;

    fn enabled_config() -> CostConfig {
        CostConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn config_with_timezone(timezone: &str) -> CostConfig {
        CostConfig {
            enabled: true,
            default_timezone: timezone.to_string(),
            ..Default::default()
        }
    }

    fn write_records(path: &Path, records: &[CostRecord]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .unwrap();
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
        }
        file.sync_all().unwrap();
    }

    fn usage_at(
        model: &str,
        input: u64,
        output: u64,
        timestamp: chrono::DateTime<Utc>,
    ) -> TokenUsage {
        let mut usage = TokenUsage::new(model, input, output, 0, 1.0, 1.0, 0.0);
        usage.timestamp = timestamp;
        usage
    }

    #[test]
    fn cost_tracker_initialization() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        assert!(!tracker.session_id().is_empty());
    }

    #[test]
    fn budget_check_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: false,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, tmp.path()).unwrap();
        let check = tracker.check_budget(1000.0).unwrap();
        assert!(matches!(check, BudgetCheck::Allowed));
    }

    #[test]
    fn record_usage_and_get_summary() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let usage = TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0);
        tracker.record_usage(usage).unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!(summary.session_cost_usd > 0.0);
        assert_eq!(summary.by_model.len(), 1);
    }

    #[test]
    fn budget_exceeded_daily_limit() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            daily_limit_usd: 0.01, // Very low limit
            ..Default::default()
        };

        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        // Record a usage that exceeds the limit
        let usage = TokenUsage::new("test/model", 10000, 5000, 0, 1.0, 2.0, 0.0); // ~0.02 USD
        tracker.record_usage(usage).unwrap();

        let check = tracker.check_budget(0.01).unwrap();
        assert!(matches!(check, BudgetCheck::Exceeded { .. }));
    }

    #[test]
    fn summary_by_model_is_daily_scoped() {
        // by_model rollup pulls from today's persisted records so the
        // dashboard's per-model breakdown survives daemon restarts (matches
        // by_agent's behaviour). A record from another session that
        // happened today still shows up; only ones outside the day fall
        // off — exercised by the storage layer's get_aggregated_costs.
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let prior_today = CostRecord::new(
            "prior-session",
            TokenUsage::new("prior/model", 500, 500, 0, 1.0, 1.0, 0.0),
        );
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(storage_path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&prior_today).unwrap()).unwrap();
        file.sync_all().unwrap();

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        tracker
            .record_usage(TokenUsage::new(
                "session/model",
                1000,
                1000,
                0,
                1.0,
                1.0,
                0.0,
            ))
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(
            summary.by_model.len(),
            2,
            "by_model must include every model that recorded today, \
             regardless of which session wrote the record"
        );
        assert!(summary.by_model.contains_key("session/model"));
        assert!(summary.by_model.contains_key("prior/model"));
    }

    #[test]
    fn malformed_lines_are_ignored_while_loading() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let valid_usage = TokenUsage::new("test/model", 1000, 0, 0, 1.0, 1.0, 0.0);
        let valid_record = CostRecord::new("session-a", valid_usage.clone());

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(storage_path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&valid_record).unwrap()).unwrap();
        writeln!(file, "not-a-json-line").unwrap();
        writeln!(file).unwrap();
        file.sync_all().unwrap();

        let tracker = CostTracker::new(config_with_timezone("UTC"), tmp.path()).unwrap();
        let today_cost = tracker.get_daily_cost(Utc::now().date_naive()).unwrap();
        assert!((today_cost - valid_usage.cost_usd).abs() < f64::EPSILON);
    }

    #[test]
    fn per_agent_aggregation_buckets_by_alias() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 1_000, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 2_000, 0, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 500, 500, 0, 1.0, 1.0, 0.0),
                Some("scribe"),
            )
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.by_agent.len(), 2);
        let scout = summary.by_agent.get("scout").unwrap();
        assert_eq!(scout.request_count, 2);
        assert_eq!(scout.total_tokens, 4_000);
        let scribe = summary.by_agent.get("scribe").unwrap();
        assert_eq!(scribe.request_count, 1);
        assert_eq!(scribe.total_tokens, 1_000);

        let scoped = tracker.get_summary_for_agent("scout").unwrap();
        assert_eq!(scoped.request_count, 2);
        assert!(
            scoped.by_agent.is_empty(),
            "per-agent view doesn't re-bucket"
        );
        assert!(
            (scoped.daily_cost_usd - scout.cost_usd).abs() < 1e-9,
            "daily filtered to alias must match by_agent bucket"
        );
    }

    #[test]
    fn track_per_agent_disabled_strips_alias() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            track_per_agent: false,
            ..Default::default()
        };
        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 1_000, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!(
            summary.by_agent.is_empty(),
            "track_per_agent=false must not surface per-agent rollups"
        );
    }

    #[test]
    fn invalid_budget_estimate_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let err = tracker.check_budget(f64::NAN).unwrap_err();
        assert!(
            err.to_string()
                .contains("Estimated cost must be a finite, non-negative value")
        );
    }

    #[test]
    fn token_summary_day_uses_configured_timezone_bucket() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        let before_midnight_utc = Utc.with_ymd_and_hms(2026, 6, 8, 15, 59, 0).unwrap();
        let after_midnight_utc = Utc.with_ymd_and_hms(2026, 6, 8, 16, 1, 0).unwrap();
        write_records(
            &storage_path,
            &[
                CostRecord::new(
                    "session-a",
                    usage_at("tz/model-before", 100, 10, before_midnight_utc),
                ),
                CostRecord::new(
                    "session-b",
                    usage_at("tz/model-after", 200, 20, after_midnight_utc),
                ),
            ],
        );

        let tracker = CostTracker::new(config_with_timezone("Asia/Shanghai"), tmp.path()).unwrap();

        let june_8 = tracker
            .get_token_summary_day(NaiveDate::from_ymd_opt(2026, 6, 8).unwrap(), None)
            .unwrap();
        assert_eq!(june_8.tz, "Asia/Shanghai");
        assert!(june_8.models.contains_key("tz/model-before"));
        assert!(!june_8.models.contains_key("tz/model-after"));

        let june_9 = tracker
            .get_token_summary_day(NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(), None)
            .unwrap();
        assert_eq!(june_9.tz, "Asia/Shanghai");
        assert!(!june_9.models.contains_key("tz/model-before"));
        assert!(june_9.models.contains_key("tz/model-after"));
    }

    #[test]
    fn storage_initialization_rebuilds_aggregates_when_timezone_changes() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        let aggregate_path = storage_path.with_file_name("cost_aggregates.json");
        let record = CostRecord::new(
            "session-a",
            usage_at(
                "tz/rebuild-model",
                123,
                45,
                Utc.with_ymd_and_hms(2026, 6, 8, 16, 30, 0).unwrap(),
            ),
        );
        write_records(&storage_path, &[record]);

        let stale = CostAggregates {
            tz: "UTC".to_string(),
            daily: HashMap::from([(
                "2026-06-08".to_string(),
                HashMap::from([(
                    TOTAL_KEY.to_string(),
                    AggregateStats {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        billable_input_tokens: 1,
                        output_tokens: 1,
                        cost_usd: 99.0,
                        request_count: 1,
                    },
                )]),
            )]),
            monthly: HashMap::new(),
        };
        fs::write(&aggregate_path, serde_json::to_vec(&stale).unwrap()).unwrap();

        let storage =
            CostStorage::new(&storage_path, &config_with_timezone("Asia/Shanghai")).unwrap();
        assert_eq!(storage.timezone_name(), "Asia/Shanghai");
        assert!(
            storage.aggregates.daily.contains_key("2026-06-09"),
            "aggregate should rebuild into the configured timezone day"
        );
        assert_eq!(
            storage
                .aggregates
                .daily
                .get("2026-06-09")
                .and_then(|v| v.get(TOTAL_KEY))
                .map(|v| v.request_count),
            Some(1)
        );

        let persisted: CostAggregates =
            serde_json::from_slice(&fs::read(&aggregate_path).unwrap()).unwrap();
        assert_eq!(persisted.tz, "Asia/Shanghai");
        assert!(persisted.daily.contains_key("2026-06-09"));
    }

    #[test]
    fn storage_prunes_detail_and_aggregate_windows_to_config_limits() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        let records = vec![
            CostRecord::new(
                "s1",
                usage_at(
                    "model-1",
                    10,
                    1,
                    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap(),
                ),
            ),
            CostRecord::new(
                "s2",
                usage_at(
                    "model-2",
                    20,
                    2,
                    Utc.with_ymd_and_hms(2026, 6, 2, 12, 0, 0).unwrap(),
                ),
            ),
            CostRecord::new(
                "s3",
                usage_at(
                    "model-3",
                    30,
                    3,
                    Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap(),
                ),
            ),
            CostRecord::new(
                "s4",
                usage_at(
                    "model-4",
                    40,
                    4,
                    Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap(),
                ),
            ),
        ];
        write_records(&storage_path, &records);

        let mut config = config_with_timezone("UTC");
        config.storage.max_detail_records = 2;
        config.storage.retain_daily_days = 2;
        config.storage.retain_monthly_months = 1;
        config.storage.max_total_bytes = u64::MAX;

        let storage = CostStorage::new(&storage_path, &config).unwrap();

        let detail_lines = storage.read_detail_lines().unwrap();
        assert_eq!(
            detail_lines.len(),
            2,
            "detail retention should keep the newest N records"
        );

        let retained_daily: Vec<_> = storage.aggregates.daily.keys().cloned().collect();
        assert_eq!(retained_daily.len(), 2);
        assert!(retained_daily.contains(&"2026-06-03".to_string()));
        assert!(retained_daily.contains(&"2026-07-01".to_string()));

        let retained_monthly: Vec<_> = storage.aggregates.monthly.keys().cloned().collect();
        assert_eq!(retained_monthly, vec!["2026-07".to_string()]);
    }
}
