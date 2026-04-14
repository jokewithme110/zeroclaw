use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    PendingScan,
    Scanning,
    Allowed,
    Blocked,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillScanRecord {
    pub skill_id: String,
    pub archive_sha256: String,
    pub scan_task_no: Option<String>,
    pub scan_status: ScanStatus,
    pub max_severity: Option<String>,
    pub is_safe: Option<bool>,
    pub last_scanned_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UploadResult {
    pub task_no: String,
    pub file_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub status: i32,
    pub status_text: String,
    pub is_safe: Option<bool>,
    pub max_severity: Option<String>,
    pub file_sha256: Option<String>,
    pub analysis_level: Option<String>,
    pub analysis_reason: Option<String>,
    pub analysis_suggestion: Option<String>,
}
