use super::archive::{build_skill_archive, sha256_hex};
use super::client::SkillScanClient;
use super::policy::is_allowed_by_severity;
use super::store::SkillScanStore;
use super::types::{ScanStatus, SkillScanRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use zeroclaw_config::schema::{SkillScanSeverity, SkillsScanConfig};

const MAX_UPLOAD_THREADS: usize = 4;

pub fn run_scan_cycle(workspace_dir: &Path, scan_cfg: &SkillsScanConfig) -> Result<()> {
    if !scan_cfg.enabled {
        return Ok(());
    }
    let mut store = SkillScanStore::load(workspace_dir)?;
    let skill_dirs = list_workspace_skill_dirs(workspace_dir)?;
    tracing::debug!(
        "skill scan cycle: discovered {} skill directories under {}",
        skill_dirs.len(),
        workspace_dir.join("skills").display()
    );
    let active_skill_ids: HashSet<String> = skill_dirs
        .iter()
        .map(|dir| skill_id_from_dir(dir))
        .collect();
    let pruned = store.prune_missing_skills(&active_skill_ids);
    if pruned > 0 {
        tracing::info!("skill scan: removed {pruned} stale records for deleted skills");
    }
    run_parallel_upload_phase(&skill_dirs, scan_cfg, &mut store)?;
    for skill_dir in skill_dirs {
        let skill_id = skill_id_from_dir(&skill_dir);
        if let Err(err) = poll_skill_once(&skill_id, scan_cfg, &mut store) {
            tracing::warn!("skill scan: poll failed for '{skill_id}': {err:#}");
        }
    }
    store.save()?;
    Ok(())
}

#[derive(Debug)]
enum UploadOutcome {
    Skipped {
        skill_id: String,
    },
    Uploaded {
        skill_id: String,
        archive_sha: String,
        task_no: String,
    },
    Failed {
        skill_id: String,
        archive_sha: String,
        error: String,
    },
}

fn run_parallel_upload_phase(
    skill_dirs: &[PathBuf],
    scan_cfg: &SkillsScanConfig,
    store: &mut SkillScanStore,
) -> Result<()> {
    for chunk in skill_dirs.chunks(MAX_UPLOAD_THREADS) {
        let mut handles = Vec::with_capacity(chunk.len());
        for skill_dir in chunk {
            let skill_dir = skill_dir.clone();
            let scan_cfg = scan_cfg.clone();
            let existing = store.get(&skill_id_from_dir(&skill_dir)).cloned();
            let handle = std::thread::spawn(move || {
                prepare_and_upload_skill(&skill_dir, &scan_cfg, existing)
            });
            handles.push(handle);
        }

        for handle in handles {
            let outcome = match handle.join() {
                Ok(outcome) => outcome,
                Err(_) => UploadOutcome::Failed {
                    skill_id: "<unknown>".to_string(),
                    archive_sha: String::new(),
                    error: "upload worker thread panicked".to_string(),
                },
            };
            match outcome {
                UploadOutcome::Skipped { skill_id } => {
                    tracing::debug!("skill scan: skip upload for '{skill_id}'");
                }
                UploadOutcome::Uploaded {
                    skill_id,
                    archive_sha,
                    task_no,
                } => {
                    let record = SkillScanRecord {
                        skill_id: skill_id.clone(),
                        archive_sha256: archive_sha,
                        scan_task_no: Some(task_no.clone()),
                        // Keep blocked by default until a later polling cycle decides final status.
                        scan_status: ScanStatus::Blocked,
                        max_severity: None,
                        is_safe: None,
                        last_scanned_at: None,
                        last_error: Some("scan queued, awaiting result polling".to_string()),
                    };
                    store.upsert(record);
                    // Persist immediately once task_no is available.
                    store.save()?;
                    tracing::info!(
                        "skill scan: upload accepted for '{}' task_no={} (persisted)",
                        skill_id,
                        task_no
                    );
                }
                UploadOutcome::Failed {
                    skill_id,
                    archive_sha,
                    error,
                } => {
                    let record = SkillScanRecord {
                        skill_id: skill_id.clone(),
                        archive_sha256: archive_sha,
                        scan_task_no: None,
                        scan_status: ScanStatus::Error,
                        max_severity: None,
                        is_safe: None,
                        last_scanned_at: Some(Utc::now().to_rfc3339()),
                        last_error: Some(error.clone()),
                    };
                    store.upsert(record);
                    store.save()?;
                    tracing::warn!("skill scan: upload failed for '{skill_id}': {error}");
                }
            }
        }
    }

    Ok(())
}

fn prepare_and_upload_skill(
    skill_dir: &Path,
    scan_cfg: &SkillsScanConfig,
    existing: Option<SkillScanRecord>,
) -> UploadOutcome {
    let skill_id = skill_id_from_dir(skill_dir);
    let archive_bytes = match build_skill_archive(skill_dir) {
        Ok(bytes) => bytes,
        Err(err) => {
            return UploadOutcome::Failed {
                skill_id,
                archive_sha: String::new(),
                error: format!("build archive failed: {err:#}"),
            };
        }
    };
    let archive_sha = sha256_hex(&archive_bytes);

    if let Some(existing) = existing.as_ref() {
        if existing.archive_sha256 == archive_sha {
            if matches!(existing.scan_status, ScanStatus::Allowed) {
                return UploadOutcome::Skipped { skill_id };
            }
            if existing.scan_task_no.is_some() && should_poll_record(existing) {
                return UploadOutcome::Skipped { skill_id };
            }
            if !should_retry_upload(existing) {
                return UploadOutcome::Skipped { skill_id };
            }
            tracing::info!(
                "skill scan: retry upload for '{}' with unchanged archive due to status={:?}, last_error={:?}",
                skill_id,
                existing.scan_status,
                existing.last_error
            );
        }
        if existing.archive_sha256 != archive_sha {
            tracing::info!(
                "skill scan: archive changed for '{}' (old_sha={}, new_sha={})",
                skill_id,
                existing.archive_sha256,
                archive_sha
            );
        }
    }

    let client = match SkillScanClient::new(
        scan_cfg.api.upload_url.clone(),
        scan_cfg.api.result_url.clone(),
    ) {
        Ok(client) => client,
        Err(err) => {
            return UploadOutcome::Failed {
                skill_id,
                archive_sha,
                error: format!("build client failed: {err:#}"),
            };
        }
    };
    let filename = format!("{skill_id}.zip");
    let upload = match client.upload_archive(archive_bytes, &filename) {
        Ok(upload) => upload,
        Err(err) => {
            return UploadOutcome::Failed {
                skill_id,
                archive_sha,
                error: format!("upload scan failed: {err:#}"),
            };
        }
    };
    if let Some(server_sha) = upload.file_sha256.as_deref() {
        if !server_sha.eq_ignore_ascii_case(&archive_sha) {
            return UploadOutcome::Failed {
                skill_id,
                archive_sha: archive_sha.clone(),
                error: format!(
                    "archive sha mismatch: local={}, remote={server_sha}",
                    archive_sha
                ),
            };
        }
    }
    UploadOutcome::Uploaded {
        skill_id,
        archive_sha,
        task_no: upload.task_no,
    }
}

fn should_retry_upload(existing: &SkillScanRecord) -> bool {
    matches!(
        existing.scan_status,
        ScanStatus::Scanning | ScanStatus::PendingScan | ScanStatus::Error
    ) || (matches!(existing.scan_status, ScanStatus::Blocked)
        && existing.last_error.as_deref().is_some_and(|e| {
            e.contains("timed out")
                || e.contains("failed")
                || e.contains("queued")
                || e.contains("awaiting result")
                || e.contains("still running")
        }))
}

fn should_poll_record(existing: &SkillScanRecord) -> bool {
    matches!(
        existing.scan_status,
        ScanStatus::Scanning | ScanStatus::PendingScan
    ) || existing.last_error.as_deref().is_some_and(|e| {
        e.contains("timed out")
            || e.contains("queued")
            || e.contains("awaiting result")
            || e.contains("still running")
    })
}

fn poll_skill_once(
    skill_id: &str,
    scan_cfg: &SkillsScanConfig,
    store: &mut SkillScanStore,
) -> Result<()> {
    let Some(existing) = store.get(skill_id).cloned() else {
        return Ok(());
    };
    if !should_poll_record(&existing) {
        return Ok(());
    }
    let Some(task_no) = existing.scan_task_no.clone() else {
        return Ok(());
    };
    let client = SkillScanClient::new(
        scan_cfg.api.upload_url.clone(),
        scan_cfg.api.result_url.clone(),
    )?;
    let result = client
        .query_result(&task_no)
        .with_context(|| format!("query result failed for task {task_no}"))?;
    let mut record = existing;
    if let Some(server_sha) = result.file_sha256.as_deref() {
        if !server_sha.eq_ignore_ascii_case(&record.archive_sha256) {
            record.scan_status = ScanStatus::Blocked;
            record.last_error = Some(format!(
                "archive sha mismatch while polling: local={}, remote={server_sha}",
                record.archive_sha256
            ));
            record.last_scanned_at = Some(Utc::now().to_rfc3339());
            store.upsert(record);
            return Ok(());
        }
    }
    let completed = result.status == 2 || result.status_text == "completed";
    if !completed {
        record.scan_status = ScanStatus::Blocked;
        record.last_error = Some(format!("scan still running (task_no={task_no})"));
        store.upsert(record);
        return Ok(());
    }

    let raw_max_severity = result.max_severity.clone();
    let parsed_severity = raw_max_severity
        .as_deref()
        .and_then(SkillScanSeverity::parse);
    if raw_max_severity.is_some() && parsed_severity.is_none() {
        tracing::warn!(
            "skill scan: '{}' returned unsupported max_severity={:?}; falling back to is_safe-driven decision",
            skill_id,
            raw_max_severity
        );
    }

    let severity = parsed_severity
        .or_else(|| {
            if result.is_safe.unwrap_or(false) {
                Some(SkillScanSeverity::Safe)
            } else {
                None
            }
        })
        .unwrap_or(SkillScanSeverity::Critical);
    let allowed = is_allowed_by_severity(scan_cfg.max_allowed_severity, severity);
    record.scan_status = if allowed {
        ScanStatus::Allowed
    } else {
        ScanStatus::Blocked
    };
    record.max_severity = raw_max_severity
        .clone()
        .or_else(|| Some(format!("{severity:?}").to_ascii_uppercase()));
    record.is_safe = result.is_safe;
    record.last_scanned_at = Some(Utc::now().to_rfc3339());
    record.last_error = None;
    store.upsert(record);
    tracing::info!(
        "skill scan: decision for '{}' => {} (task_no={}, raw_max_severity={:?}, effective_severity={:?}, is_safe={:?})",
        skill_id,
        if allowed { "allowed" } else { "blocked" },
        task_no,
        raw_max_severity,
        severity,
        result.is_safe
    );
    Ok(())
}

fn list_workspace_skill_dirs(workspace_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = workspace_dir.join("skills");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut dirs = Vec::new();
    for entry in
        std::fs::read_dir(&root).with_context(|| format!("failed to read {}", root.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

pub fn skill_id_from_dir(skill_dir: &Path) -> String {
    skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| skill_dir.display().to_string())
}
