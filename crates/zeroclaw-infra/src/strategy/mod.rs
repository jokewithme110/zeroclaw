use std::path::PathBuf;
use std::time::SystemTime;

/// 文件元数据（抽象层，便于测试）
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub size_bytes: u64,
}

/// 策略配置
#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub retention_hours: u64,
    pub max_size_bytes: u64,
    pub current_dir_size_bytes: u64,
}

/// 清理策略接口
pub trait CleanupStrategy: Send + Sync {
    /// 返回需要删除的文件列表
    fn find_files_to_delete(&self, files: &[FileMeta], config: &StrategyConfig) -> Vec<PathBuf>;
}

/// 空间超限策略
pub mod space_based;
/// 时间到期策略
pub mod time_based;

pub use space_based::SpaceBasedStrategy;
pub use time_based::TimeBasedStrategy;
