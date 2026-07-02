use crate::cleanup_rule::{CleanupRule, is_protected_path};
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 文件分类标签（用于精确匹配）
#[derive(Debug, Clone, PartialEq)]
pub enum TempFileCategory {
    QqAttachments,
    WeChatAttachments,
    DingTalkAttachments,
    LarkAttachments,
    NodeCameraSnaps,
    Custom(String),
}

/// 执行报告
pub struct EnforceReport {
    pub rules_executed: usize,
    pub files_deleted: usize,
    pub bytes_freed: u64,
    pub errors: Vec<(String, String)>, // (rule_path, error_msg)
}

/// 使用情况信息
pub struct UsageInfo {
    pub total_size_mb: f64,
    pub file_count: usize,
    pub oldest_file_age_hours: f64,
    pub newest_file_age_hours: f64,
}

/// 临时文件管理器配置
#[derive(Clone)]
pub struct TempFileConfig {
    /// 是否启用自动清理（默认 true）
    pub enabled: bool,

    /// 内置规则快捷配置（向后兼容）
    pub temp_file_retention_hours: u64,
    pub temp_file_max_size_mb: u64,

    /// 是否启用定时清理（默认 false）
    pub scheduled_cleanup_enabled: bool,

    /// 定时清理间隔（小时，支持小数，默认 1.0）
    pub scheduled_cleanup_interval_hours: f64,

    /// 用户自定义规则列表
    pub rules: Vec<TempCleanupRule>,
}

/// 单条清理规则配置
#[derive(Clone)]
pub struct TempCleanupRule {
    /// 相对于 workspace 的路径（如 "qq_files/", "logs/"）
    pub path: String,

    /// 文件名 glob 模式（如 "*.log", "node_snap_*"），None 表示匹配全部
    pub pattern: Option<String>,

    /// 保留时间（小时），0 = 不限制时间
    pub retention_hours: u64,

    /// 目录最大总大小（MB），0 = 不限制空间
    pub max_size_mb: u64,
}

impl Default for TempFileConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            temp_file_retention_hours: 24,
            temp_file_max_size_mb: 50,
            scheduled_cleanup_enabled: false,
            scheduled_cleanup_interval_hours: 1.0,
            rules: vec![],
        }
    }
}

use tokio_util::sync::CancellationToken;

fn temp_file_event(attrs: serde_json::Value) -> zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(attrs)
}

pub struct TempFileManager {
    workspace_root: PathBuf,
    rules: Vec<Arc<CleanupRule>>,
    enabled: bool,
    /// Scheduled cleanup task configuration
    scheduled_cleanup_enabled: bool,
    scheduled_cleanup_interval_hours: f64,
}

impl TempFileManager {
    /// Built-in shortcut rules keyed by their relative directory name.
    /// Behavior must stay identical to the historical hard-coded list:
    /// `qq_files/`, `wechat_files/`, `dingtalk_files/`, `lark_files/`
    /// use the universal retention/size with a `*` pattern, while
    /// `media/` keeps its special `node_snap_*` pattern. New channels
    /// that follow the `<id>_files/` convention pick up the universal
    /// limits automatically without a code change.
    const BUILTIN_DIR_PATTERNS: &'static [(&'static str, Option<&'static str>)] = &[
        ("qq_files/", Some("*")),
        ("wechat_files/", Some("*")),
        ("dingtalk_files/", Some("*")),
        ("lark_files/", Some("*")),
        ("media/", Some("node_snap_*")),
    ];

    /// Derive the channel-specific `<id>_files/` rule for a given file path.
    /// Returns `Some(rule)` only when the file's first workspace-relative
    /// segment ends with `_files`, so e.g. `attachments/telegram/...` does
    /// not accidentally pick up a `telegram` channel mapping. The five
    /// built-in entries above are excluded so they keep their declared
    /// patterns (e.g. `media/node_snap_*`).
    fn channel_builtin_rule(
        workspace_root: &Path,
        file_path: &Path,
        retention_hours: u64,
        max_size_mb: u64,
    ) -> Option<TempCleanupRule> {
        let rel = file_path.strip_prefix(workspace_root).ok()?;
        let first = rel.components().next()?.as_os_str().to_str()?;
        if !first.ends_with("_files") {
            return None;
        }
        let dir = format!("{}/", first);
        if Self::BUILTIN_DIR_PATTERNS.iter().any(|(p, _)| *p == dir) {
            return None;
        }
        Some(TempCleanupRule {
            path: dir,
            pattern: Some("*".to_string()),
            retention_hours,
            max_size_mb,
        })
    }

    /// Pick a generic fallback rule for a file that did not match any
    /// built-in channel convention and has no explicit `files_cleanup.rules`
    /// entry. Uses the file's first workspace-relative directory as the
    /// cleanup target so any channel's attachments end up governed by the
    /// universal retention/size limits. Returns `None` when the file is
    /// not under the workspace, sits directly at the workspace root, or
    /// lives inside a protected system directory.
    fn generic_fallback_rule(
        workspace_root: &Path,
        file_path: &Path,
        retention_hours: u64,
        max_size_mb: u64,
    ) -> Option<TempCleanupRule> {
        let rel = file_path.strip_prefix(workspace_root).ok()?;
        let first = rel.components().next()?.as_os_str().to_str()?;
        if first.is_empty() {
            return None;
        }
        let dir = format!("{}/", first);
        if is_protected_path(workspace_root, &dir) {
            return None;
        }
        Some(TempCleanupRule {
            path: dir,
            pattern: Some("*".to_string()),
            retention_hours,
            max_size_mb,
        })
    }

    /// Trigger cleanup for one freshly written file using the canonical cleanup config.
    ///
    /// `include_custom_rules` controls whether the user's
    /// `files_cleanup.rules` entries participate in the rule-resolution
    /// walk. Callers on the inbound (channel-message) path pass `false`
    /// so that those rules — which are written against the global
    /// `data_dir` workspace and reserved for the scheduled scan — don't
    /// apply. Callers on the scheduled-scan and manual-cleanup paths
    /// pass `true` to keep the historical behavior.
    pub fn trigger_cleanup_by_path(
        workspace_root: &Path,
        file_path: &Path,
        config: &TempFileConfig,
        include_custom_rules: bool,
    ) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }

        let mut rule_configs = Vec::new();
        for (dir, pattern) in Self::BUILTIN_DIR_PATTERNS {
            let builtin_abs = workspace_root.join(dir);
            if file_path.starts_with(&builtin_abs) {
                rule_configs.push(TempCleanupRule {
                    path: (*dir).to_string(),
                    pattern: pattern.map(str::to_string),
                    retention_hours: config.temp_file_retention_hours,
                    max_size_mb: config.temp_file_max_size_mb,
                });
            }
        }

        if let Some(channel_rule) = Self::channel_builtin_rule(
            workspace_root,
            file_path,
            config.temp_file_retention_hours,
            config.temp_file_max_size_mb,
        ) {
            rule_configs.push(channel_rule);
        }

        if include_custom_rules {
            for custom_rule in &config.rules {
                if file_path.starts_with(workspace_root.join(&custom_rule.path)) {
                    rule_configs.push(custom_rule.clone());
                }
            }
        }

        if rule_configs.is_empty()
            && let Some(fallback) = Self::generic_fallback_rule(
                workspace_root,
                file_path,
                config.temp_file_retention_hours,
                config.temp_file_max_size_mb,
            )
        {
            rule_configs.push(fallback);
        }

        for rule_config in rule_configs {
            match CleanupRule::new(
                workspace_root,
                &rule_config.path,
                rule_config.pattern.as_deref(),
                rule_config.retention_hours,
                rule_config.max_size_mb,
            ) {
                Ok(rule) => {
                    if let Err(error) = rule.register_and_enforce(file_path) {
                        ::zeroclaw_log::record!(
                            WARN,
                            temp_file_event(json!({
                                "rule_path": rule.path_display(),
                                "file_path": file_path.display().to_string(),
                                "error": error.to_string()
                            }))
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                            "Cleanup failed after registering file"
                        );
                    }
                }
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        temp_file_event(json!({
                            "rule_path": rule_config.path,
                            "file_path": file_path.display().to_string(),
                            "error": error.to_string()
                        }))
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "Failed to create cleanup rule for triggered cleanup"
                    );
                }
            }
        }

        Ok(())
    }

    /// 从配置创建管理器
    pub fn from_config(workspace_root: PathBuf, config: &TempFileConfig) -> Result<Self> {
        if !config.enabled {
            ::zeroclaw_log::record!(
                INFO,
                temp_file_event(json!({"enabled": false})),
                "Temporary file cleanup is disabled"
            );
            return Ok(Self {
                workspace_root,
                rules: vec![],
                enabled: false,
                scheduled_cleanup_enabled: config.scheduled_cleanup_enabled,
                scheduled_cleanup_interval_hours: config.scheduled_cleanup_interval_hours,
            });
        }

        let mut rules = Vec::new();

        // 添加用户自定义规则（跳过受保护的系统目录）。
        // 注：内置规则（qq_files/、wechat_files/ 等）不再注册到
        // `self.rules`，因此不再随定时扫描自动执行。消息触发清理
        // 仍由 `BUILTIN_DIR_PATTERNS` 在 `trigger_cleanup_by_path`
        // 中按需提供，与本扫描路径解耦。
        for rule_config in &config.rules {
            // 检查是否是受保护的目录
            if is_protected_path(&workspace_root, &rule_config.path) {
                ::zeroclaw_log::record!(
                    WARN,
                    temp_file_event(json!({"rule_path": rule_config.path}))
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Skipping protected system directory from cleanup rules"
                );
                continue;
            }

            match CleanupRule::new(
                &workspace_root,
                &rule_config.path,
                rule_config.pattern.as_deref(),
                rule_config.retention_hours,
                rule_config.max_size_mb,
            ) {
                Ok(rule) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({
                            "rule_path": rule.path_display(),
                            "retention_hours": rule_config.retention_hours,
                            "max_size_mb": rule_config.max_size_mb
                        })),
                        "Registered custom cleanup rule"
                    );
                    rules.push(Arc::new(rule));
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        temp_file_event(json!({
                            "rule_path": rule_config.path,
                            "error": e.to_string()
                        }))
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "Failed to create custom cleanup rule"
                    );
                }
            }
        }

        Ok(Self {
            workspace_root,
            rules,
            enabled: true,
            scheduled_cleanup_enabled: config.scheduled_cleanup_enabled,
            scheduled_cleanup_interval_hours: config.scheduled_cleanup_interval_hours,
        })
    }

    /// 登记文件并触发对应规则的清理
    pub fn register(&self, path: &Path, _category: Option<TempFileCategory>) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // 找到匹配的规则并执行清理
        for rule in &self.rules {
            if rule.matches(path) {
                match rule.register_and_enforce(path) {
                    Ok(deleted) => {
                        if !deleted.is_empty() {
                            ::zeroclaw_log::record!(
                                INFO,
                                temp_file_event(json!({
                                    "registered_file": path.display().to_string(),
                                    "deleted_count": deleted.len()
                                })),
                                "Registered file and triggered cleanup"
                            );
                        }
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            temp_file_event(json!({
                                "registered_file": path.display().to_string(),
                                "error": e.to_string()
                            }))
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                            "Cleanup failed after registering file"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// 手动执行所有规则的清理（供 CLI 命令调用）。
    ///
    /// 仅遍历 `self.rules`，即用户在 `files_cleanup.rules`
    /// 中显式声明的规则。历史内置的 `qq_files/`、`wechat_files/`、
    /// `dingtalk_files/`、`lark_files/`、`media/node_snap_*`
    /// 不再随本扫描执行 —— 它们的消息触发清理由
    /// `trigger_cleanup_by_path` 在 C 路径上处理。
    pub fn enforce_all(&self) -> Result<EnforceReport> {
        let mut report = EnforceReport {
            rules_executed: 0,
            files_deleted: 0,
            bytes_freed: 0,
            errors: vec![],
        };

        ::zeroclaw_log::record!(
            DEBUG,
            temp_file_event(json!({"rules_count": self.rules.len()})),
            "Starting cleanup scan"
        );

        for rule in &self.rules {
            report.rules_executed += 1;
            match rule.enforce() {
                Ok(deleted) => {
                    let count = deleted.len();
                    // 估算释放的空间（文件已删除，只能估算）
                    report.files_deleted += count;
                }
                Err(e) => {
                    report.errors.push((rule.path_display(), e.to_string()));
                }
            }
        }

        Ok(report)
    }

    /// 查询某目录的使用情况（供 LLM 查询）
    pub fn get_usage(&self, category: &TempFileCategory) -> Result<UsageInfo> {
        use crate::dir_monitor::DirMonitor;

        let rel_path = match category {
            TempFileCategory::QqAttachments => "qq_files",
            TempFileCategory::WeChatAttachments => "wechat_files",
            TempFileCategory::DingTalkAttachments => "dingtalk_files",
            TempFileCategory::LarkAttachments => "lark_files",
            TempFileCategory::NodeCameraSnaps => "media",
            TempFileCategory::Custom(name) => name,
        };

        let dir = self.workspace_root.join(rel_path);
        if !dir.exists() {
            return Ok(UsageInfo {
                total_size_mb: 0.0,
                file_count: 0,
                oldest_file_age_hours: 0.0,
                newest_file_age_hours: 0.0,
            });
        }

        let files = DirMonitor::enumerate_files(&dir, None)?;
        let total_size_bytes: u64 = files.iter().map(|f| f.size_bytes).sum();
        let total_size_mb = total_size_bytes as f64 / 1024.0 / 1024.0;

        let now = std::time::SystemTime::now();
        let ages_hours: Vec<f64> = files
            .iter()
            .filter_map(|f| {
                f.mtime
                    .duration_since(now)
                    .ok()
                    .map(|d| d.as_secs_f64() / 3600.0)
            })
            .collect();

        let oldest = ages_hours.iter().cloned().fold(f64::NAN, f64::max);
        let newest = ages_hours.iter().cloned().fold(f64::NAN, f64::min);

        Ok(UsageInfo {
            total_size_mb,
            file_count: files.len(),
            oldest_file_age_hours: if oldest.is_nan() { 0.0 } else { oldest },
            newest_file_age_hours: if newest.is_nan() { 0.0 } else { newest },
        })
    }

    /// 获取规则数量
    pub fn rules_count(&self) -> usize {
        self.rules.len()
    }

    /// 检查是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 检查定时清理是否启用
    pub fn scheduled_cleanup_enabled(&self) -> bool {
        self.scheduled_cleanup_enabled
    }

    /// 获取定时清理间隔（小时）
    pub fn scheduled_cleanup_interval_hours(&self) -> f64 {
        self.scheduled_cleanup_interval_hours
    }

    /// Calculate the interval duration with minimum enforcement (1 minute)
    fn calculate_interval_duration(&self) -> std::time::Duration {
        let minutes = self.scheduled_cleanup_interval_hours * 60.0;

        // Minimum enforcement: not less than 1 minute
        let effective_minutes = if minutes < 1.0 {
            ::zeroclaw_log::record!(
                WARN,
                temp_file_event(json!({
                    "configured_hours": self.scheduled_cleanup_interval_hours
                }))
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Cleanup interval too small (< 1 minute), adjusted to 1 minute"
            );
            1.0
        } else {
            minutes
        };

        std::time::Duration::from_secs_f64(effective_minutes * 60.0)
    }

    /// 启动定时清理任务（后台运行）
    pub async fn start_scheduled_cleanup(
        self: Arc<Self>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        if !self.scheduled_cleanup_enabled || !self.enabled {
            ::zeroclaw_log::record!(
                DEBUG,
                temp_file_event(json!({
                    "scheduled_cleanup_enabled": self.scheduled_cleanup_enabled,
                    "enabled": self.enabled
                })),
                "Scheduled cleanup is disabled"
            );
            return Ok(());
        }

        let interval = self.calculate_interval_duration();
        let interval_minutes = self.scheduled_cleanup_interval_hours * 60.0;
        ::zeroclaw_log::record!(
            INFO,
            temp_file_event(json!({
                "interval_hours": self.scheduled_cleanup_interval_hours,
                "interval_minutes": interval_minutes
            })),
            "Starting scheduled cleanup task"
        );

        // 启动时立即执行一次
        ::zeroclaw_log::record!(
            INFO,
            temp_file_event(json!({})),
            "Running initial startup cleanup scan"
        );

        match self.enforce_all() {
            Ok(report) => {
                if report.files_deleted > 0 {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({
                            "rules_executed": report.rules_executed,
                            "files_deleted": report.files_deleted,
                            "bytes_freed": report.bytes_freed
                        })),
                        "Startup cleanup completed"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({"rules_scanned": report.rules_executed})),
                        "Startup cleanup scan completed, no files matched deletion criteria"
                    );
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    temp_file_event(json!({"error": e.to_string()}))
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Startup cleanup failed"
                );
            }
        }

        // 然后进入定时循环
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({})),
                        "Starting scheduled cleanup scan"
                    );

                    match self.enforce_all() {
                        Ok(report) => {
                            if report.files_deleted > 0 {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    temp_file_event(json!({
                                        "rules_executed": report.rules_executed,
                                        "files_deleted": report.files_deleted,
                                        "bytes_freed": report.bytes_freed
                                    })),
                                    "Scheduled cleanup completed"
                                );
                            } else {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    temp_file_event(json!({"rules_scanned": report.rules_executed})),
                                    "Scheduled cleanup scan completed, no files matched deletion criteria"
                                );
                            }
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                temp_file_event(json!({"error": e.to_string()}))
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                                "Scheduled cleanup failed"
                            );
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    ::zeroclaw_log::record!(
                        INFO,
                        temp_file_event(json!({})),
                        "Scheduled cleanup task cancelled"
                    );
                    break;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
