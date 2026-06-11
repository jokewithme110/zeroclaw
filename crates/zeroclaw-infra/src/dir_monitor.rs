use crate::strategy::FileMeta;
use anyhow::{Context, Result};
use glob::Pattern;
use std::path::Path;
use std::time::SystemTime;

pub struct DirMonitor;

impl DirMonitor {
    /// 枚举目录下所有匹配的文件
    pub fn enumerate_files(dir: &Path, pattern: Option<&Pattern>) -> Result<Vec<FileMeta>> {
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            // 只处理文件，跳过子目录
            if !path.is_file() {
                continue;
            }

            // 检查文件名是否匹配 glob 模式
            if let Some(pat) = pattern {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !pat.matches(file_name) {
                    continue;
                }
            }

            // 获取文件元数据
            let metadata = path.metadata()?;
            let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size_bytes = metadata.len();

            files.push(FileMeta {
                path,
                mtime,
                size_bytes,
            });
        }

        Ok(files)
    }

    /// 计算目录总大小（仅匹配的文件）
    pub fn calculate_size(dir: &Path, pattern: Option<&Pattern>) -> Result<u64> {
        let files = Self::enumerate_files(dir, pattern)?;
        Ok(files.iter().map(|f| f.size_bytes).sum())
    }
}
