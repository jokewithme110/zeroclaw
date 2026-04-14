use super::archive::{build_skill_archive, sha256_hex};
use super::client::SkillScanClient;
use super::policy::is_allowed_by_severity;
use super::store::SkillScanStore;
use super::types::{ScanStatus, SkillScanRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use zeroclaw_config::schema::{SkillScanSeverity, SkillsScanConfig};

pub fn run_scan_cycle(workspace_dir: &Path, scan_cfg: &SkillsScanConfig) -> Result<()> {
    if !scan_cfg.enabled {
        return Ok(());
    }
    let mut store = SkillScanStore::load(workspace_dir)?;
    let skill_dirs = list_workspace_skill_dirs(workspace_dir)?;
    tracing::info!(
        "skill scan cycle: discovered {} skill directories under {}",
        skill_dirs.len(),
        workspace_dir.join("skills").display()
    );
    for skill_dir in skill_dirs {
        if let Err(err) = evaluate_skill(&skill_dir, scan_cfg, &mut store) {
            tracing::warn!(
                "skill scan: cycle failed for {}: {err:#}",
                skill_dir.display()
            );
        }
    }
    store.save()?;
    Ok(())
}

fn evaluate_skill(
    skill_dir: &Path,
    scan_cfg: &SkillsScanConfig,
    store: &mut SkillScanStore,
) -> Result<bool> {
    let skill_id = skill_id_from_dir(skill_dir);
    let archive_bytes = build_skill_archive(skill_dir)?;
    let archive_sha = sha256_hex(&archive_bytes);

    if let Some(existing) = store.get(&skill_id) {
        if existing.archive_sha256 == archive_sha {
            if matches!(existing.scan_status, ScanStatus::Allowed) {
                tracing::debug!(
                    "skill scan: no archive change for '{}', keeping status={:?}",
                    skill_id,
                    existing.scan_status
                );
                return Ok(true);
            }

            let should_retry_same_sha = matches!(
                existing.scan_status,
                ScanStatus::Scanning | ScanStatus::PendingScan | ScanStatus::Error
            ) || (matches!(existing.scan_status, ScanStatus::Blocked)
                && existing
                    .last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("timed out") || e.contains("failed")));

            if !should_retry_same_sha {
                tracing::debug!(
                    "skill scan: no archive change for '{}' and status is stable {:?}, skipping rescan",
                    skill_id,
                    existing.scan_status
                );
                return Ok(false);
            }

            tracing::info!(
                "skill scan: retrying '{}' with unchanged archive due to transient status={:?}, last_error={:?}",
                skill_id,
                existing.scan_status,
                existing.last_error
            );

            if let Some(existing_task_no) = existing.scan_task_no.clone() {
                tracing::info!(
                    "skill scan: reusing existing task_no={} for '{}' (no re-upload)",
                    existing_task_no,
                    skill_id
                );
                let now = Utc::now().to_rfc3339();
                let client = SkillScanClient::new(
                    scan_cfg.api.upload_url.clone(),
                    scan_cfg.api.result_url.clone(),
                )?;
                let mut record = existing.clone();
                record.scan_status = ScanStatus::Scanning;
                record.last_error = None;
                record.last_scanned_at = None;
                store.upsert(record.clone());
                return wait_for_scan_completion(
                    &client,
                    &existing_task_no,
                    &archive_sha,
                    &skill_id,
                    scan_cfg,
                    &now,
                    &mut record,
                    store,
                );
            }
        }
        if existing.archive_sha256 != archive_sha {
            tracing::info!(
                "skill scan: archive changed for '{}' (old_sha={}, new_sha={})",
                skill_id,
                existing.archive_sha256,
                archive_sha
            );
        }
    } else {
        tracing::info!(
            "skill scan: new skill detected '{}', scheduling scan",
            skill_id
        );
    }

    let now = Utc::now().to_rfc3339();
    let mut record = SkillScanRecord {
        skill_id: skill_id.clone(),
        archive_sha256: archive_sha.clone(),
        scan_task_no: None,
        scan_status: ScanStatus::Scanning,
        max_severity: None,
        is_safe: None,
        last_scanned_at: None,
        last_error: None,
    };
    store.upsert(record.clone());

    let client = SkillScanClient::new(
        scan_cfg.api.upload_url.clone(),
        scan_cfg.api.result_url.clone(),
    )?;
    let filename = format!("{skill_id}.zip");
    tracing::info!(
        "skill scan: uploading archive for '{}' as {}",
        skill_id,
        filename
    );
    let upload = client.upload_archive(archive_bytes, &filename)?;
    record.scan_task_no = Some(upload.task_no.clone());
    tracing::info!(
        "skill scan: upload accepted for '{}' task_no={}",
        skill_id,
        upload.task_no
    );
    if let Some(server_sha) = upload.file_sha256.as_deref() {
        if !server_sha.eq_ignore_ascii_case(&archive_sha) {
            record.scan_status = ScanStatus::Blocked;
            record.last_error = Some(format!(
                "archive sha mismatch: local={}, remote={server_sha}",
                archive_sha
            ));
            record.last_scanned_at = Some(now.clone());
            store.upsert(record);
            return Ok(false);
        }
    }
    store.upsert(record.clone());

    wait_for_scan_completion(
        &client,
        upload.task_no.as_str(),
        &archive_sha,
        &skill_id,
        scan_cfg,
        &now,
        &mut record,
        store,
    )
}

#[allow(clippy::too_many_arguments)]
fn wait_for_scan_completion(
    client: &SkillScanClient,
    task_no: &str,
    archive_sha: &str,
    skill_id: &str,
    scan_cfg: &SkillsScanConfig,
    now: &str,
    record: &mut SkillScanRecord,
    store: &mut SkillScanStore,
) -> Result<bool> {
    record.scan_task_no = Some(task_no.to_string());
    let deadline = Instant::now() + Duration::from_secs(scan_cfg.poll_timeout_secs.max(1));
    let poll_sleep = Duration::from_secs(scan_cfg.poll_interval_secs.max(1));
    loop {
        if Instant::now() >= deadline {
            record.scan_status = ScanStatus::Blocked;
            record.last_error = Some(format!("scan polling timed out (task_no={task_no})"));
            record.last_scanned_at = Some(now.to_string());
            store.upsert(record.clone());
            return Ok(false);
        }

        let result = client
            .query_result(task_no)
            .with_context(|| format!("query result failed for task {task_no}"))?;

        if let Some(server_sha) = result.file_sha256.as_deref() {
            if !server_sha.eq_ignore_ascii_case(archive_sha) {
                record.scan_status = ScanStatus::Blocked;
                record.last_error = Some(format!(
                    "archive sha mismatch while polling: local={}, remote={server_sha}",
                    archive_sha
                ));
                record.last_scanned_at = Some(now.to_string());
                store.upsert(record.clone());
                return Ok(false);
            }
        }

        let completed = result.status == 2 || result.status_text == "completed";
        if !completed {
            thread::sleep(poll_sleep);
            continue;
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
                    tracing::info!(
                        "skill scan: '{}' uses SAFE fallback because is_safe=true and max_severity={:?}",
                        skill_id,
                        raw_max_severity
                    );
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
        record.last_scanned_at = Some(now.to_string());
        record.last_error = None;
        store.upsert(record.clone());
        tracing::info!(
            "skill scan: decision for '{}' => {} (task_no={}, raw_max_severity={:?}, effective_severity={:?}, is_safe={:?})",
            skill_id,
            if allowed { "allowed" } else { "blocked" },
            task_no,
            raw_max_severity,
            severity,
            result.is_safe
        );
        return Ok(allowed);
    }
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
