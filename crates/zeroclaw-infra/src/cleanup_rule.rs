use crate::dir_monitor::DirMonitor;
use crate::strategy::{CleanupStrategy, SpaceBasedStrategy, StrategyConfig, TimeBasedStrategy};
use anyhow::{Context, Result};
use glob::Pattern;
use serde_json::json;
use std::path::{Path, PathBuf};

/// 系统保护目录列表
const PROTECTED_DIRS: &[&str] = &["cron", "memory", "sessions", "skills", "state"];

fn temp_file_event(attrs: serde_json::Value) -> zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(attrs)
}

/// 检查路径是否是受保护的系统目录
pub fn is_protected_path(_workspace_root: &Path, rel_path: &str) -> bool {
    // 规范化路径：移除尾部斜杠和重复斜杠
    let normalized = rel_path.trim_end_matches('/').trim_start_matches('/');

    // 检查是否是保护目录本身或其子目录
    for protected in PROTECTED_DIRS {
        if normalized == *protected || normalized.starts_with(&format!("{}/", protected)) {
            return true;
        }
    }

    // 绝对路径绕过 workspace 约束，禁止作为清理目录。
    if rel_path.starts_with('/') {
        return true;
    }

    // 检查是否是 workspace 根目录下的直接文件（非子目录）
    // 即路径中不包含任何斜杠
    if !normalized.contains('/') && !normalized.is_empty() {
        // 这是 workspace 根目录下的直接文件，需要保护
        // 但要排除已知的临时目录
        let allowed_root_dirs = [
            "qq_files",
            "wechat_files",
            "dingtalk_files",
            "lark_files",
            "media",
            "logs",
            "cache",
            "downloads",
        ];
        if !allowed_root_dirs.contains(&normalized) {
            return true;
        }
    }

    false
}

#[derive(Debug, thiserror::Error)]
pub enum RuleValidationError {
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    #[error("Invalid glob pattern: {0}")]
    InvalidGlobPattern(String),
    #[error("Both retention_hours and max_size_mb are zero")]
    NoLimits,
    #[error("Cannot add cleanup rule for protected system directory: {0}")]
    ProtectedDirectory(String),
}

pub struct CleanupRule {
    /// 绝对路径
    abs_path: PathBuf,

    /// 文件名过滤器
    pattern: Option<Pattern>,

    /// 策略配置
    retention_hours: u64,
    max_size_mb: u64,

    /// 策略执行器
    time_strategy: TimeBasedStrategy,
    space_strategy: SpaceBasedStrategy,
}

impl CleanupRule {
    pub fn new(
        workspace_root: &Path,
        rel_path: &str,
        pattern: Option<&str>,
        retention_hours: u64,
        max_size_mb: u64,
    ) -> Result<Self, RuleValidationError> {
        // 验证：至少有一个限制
        if retention_hours == 0 && max_size_mb == 0 {
            return Err(RuleValidationError::NoLimits);
        }

        // 双重检查：拒绝受保护的系统目录
        if is_protected_path(workspace_root, rel_path) {
            return Err(RuleValidationError::ProtectedDirectory(
                rel_path.to_string(),
            ));
        }

        let abs_path = workspace_root.join(rel_path);

        // 解析 glob 模式
        let parsed_pattern = pattern
            .map(Pattern::new)
            .transpose()
            .map_err(|e| RuleValidationError::InvalidGlobPattern(e.to_string()))?;

        Ok(Self {
            abs_path,
            pattern: parsed_pattern,
            retention_hours,
            max_size_mb,
            time_strategy: TimeBasedStrategy,
            space_strategy: SpaceBasedStrategy,
        })
    }

    /// 执行清理，返回删除的文件列表
    pub fn enforce(&self) -> Result<Vec<PathBuf>> {
        let mut deleted = Vec::new();

        if !self.abs_path.exists() {
            ::zeroclaw_log::record!(
                DEBUG,
                temp_file_event(json!({"path": self.abs_path.display().to_string()})),
                "Cleanup rule path does not exist, skipping"
            );
            return Ok(deleted);
        }

        // 枚举文件
        let files = DirMonitor::enumerate_files(&self.abs_path, self.pattern.as_ref())
            .with_context(|| format!("Failed to enumerate files in {}", self.abs_path.display()))?;

        if files.is_empty() {
            return Ok(deleted);
        }

        // 计算当前目录大小（字节）
        let current_size_bytes: u64 = files.iter().map(|f| f.size_bytes).sum();
        let max_size_bytes = self.max_size_mb * 1024 * 1024;

        let config = StrategyConfig {
            retention_hours: self.retention_hours,
            max_size_bytes,
            current_dir_size_bytes: current_size_bytes,
        };

        // 时间到期清理
        let expired = self.time_strategy.find_files_to_delete(&files, &config);

        if !expired.is_empty() {
            // 打印每个被时间策略清除的文件
            for file in &expired {
                ::zeroclaw_log::record!(
                    INFO,
                    temp_file_event(json!({
                        "path": file.display().to_string(),
                        "rule_path": self.abs_path.display().to_string(),
                        "strategy": "time_based",
                        "retention_hours": self.retention_hours
                    })),
                    "Deleting expired temporary file"
                );
            }
            self.delete_files(&expired)?;
            deleted.extend(expired);
        }

        // 重新枚举（因为已删除了一些文件）
        let files_after_time = DirMonitor::enumerate_files(&self.abs_path, self.pattern.as_ref())?;
        let current_size_bytes_after: u64 = files_after_time.iter().map(|f| f.size_bytes).sum();
        let max_size_bytes_after = self.max_size_mb * 1024 * 1024;

        let config_after = StrategyConfig {
            retention_hours: self.retention_hours,
            max_size_bytes: max_size_bytes_after,
            current_dir_size_bytes: current_size_bytes_after,
        };

        // 空间超限清理
        let pruned = self
            .space_strategy
            .find_files_to_delete(&files_after_time, &config_after);

        if !pruned.is_empty() {
            // 打印每个被空间策略清除的文件
            for file in &pruned {
                // 尝试获取文件大小用于日志
                let size_mb = file
                    .metadata()
                    .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                    .unwrap_or(0.0);

                ::zeroclaw_log::record!(
                    INFO,
                    temp_file_event(json!({
                        "path": file.display().to_string(),
                        "rule_path": self.abs_path.display().to_string(),
                        "strategy": "space_based",
                        "size_mb": size_mb,
                        "limit_mb": self.max_size_mb,
                        "current_size_mb": current_size_bytes_after as f64 / 1024.0 / 1024.0
                    })),
                    "Deleting temporary file due to size limit exceeded"
                );
            }
            self.delete_files(&pruned)?;
            deleted.extend(pruned);
        }

        Ok(deleted)
    }

    /// 登记文件并触发清理（写入后调用）
    pub fn register_and_enforce(&self, file_path: &Path) -> Result<Vec<PathBuf>> {
        // 检查文件是否匹配此规则
        if !self.matches(file_path) {
            return Ok(vec![]);
        }

        self.enforce()
    }

    /// 检查文件是否匹配此规则
    pub fn matches(&self, path: &Path) -> bool {
        // 检查是否在规则目录下
        if !path.starts_with(&self.abs_path) {
            return false;
        }

        // 检查文件名模式
        if let Some(ref pat) = self.pattern {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                return pat.matches(file_name);
            }
            // 如果无法获取文件名，不匹配
            return false;
        }

        // 没有模式限制，匹配
        true
    }

    /// 删除文件并记录日志
    fn delete_files(&self, paths: &[PathBuf]) -> Result<()> {
        for path in paths {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        temp_file_event(json!({"path": path.display().to_string()})),
                        "Deleted temporary file"
                    );
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        temp_file_event(
                            json!({"path": path.display().to_string(), "error": e.to_string()})
                        )
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "Failed to delete temporary file"
                    );
                }
            }
        }
        Ok(())
    }

    /// 获取规则路径的显示名称
    pub fn path_display(&self) -> String {
        self.abs_path.display().to_string()
    }
}
