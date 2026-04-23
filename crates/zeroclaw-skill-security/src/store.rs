use super::types::SkillScanRecord;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const STATE_FILE_NAME: &str = "skill_scan_state.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SkillScanState {
    #[serde(default)]
    pub records: HashMap<String, SkillScanRecord>,
}

pub struct SkillScanStore {
    path: PathBuf,
    state: SkillScanState,
}

impl SkillScanStore {
    pub fn load(workspace_dir: &Path) -> Result<Self> {
        let path = workspace_dir.join("state").join(STATE_FILE_NAME);
        let state = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            match serde_json::from_str::<SkillScanState>(&raw) {
                Ok(parsed) => parsed,
                Err(err) => {
                    tracing::warn!(
                        "failed to parse skill scan state {}; resetting: {err}",
                        path.display()
                    );
                    SkillScanState::default()
                }
            }
        } else {
            SkillScanState::default()
        };
        Ok(Self { path, state })
    }

    pub fn get(&self, skill_id: &str) -> Option<&SkillScanRecord> {
        self.state.records.get(skill_id)
    }

    pub fn upsert(&mut self, record: SkillScanRecord) {
        self.state.records.insert(record.skill_id.clone(), record);
    }

    pub fn prune_missing_skills(&mut self, active_skill_ids: &HashSet<String>) -> usize {
        let before = self.state.records.len();
        self.state
            .records
            .retain(|skill_id, _| active_skill_ids.contains(skill_id));
        before.saturating_sub(self.state.records.len())
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let backup_path = self.path.with_extension("json.bak");
        let tmp_path = self.path.with_extension("json.tmp");

        // Keep exactly one backup: replace previous backup with the latest persisted state.
        if self.path.exists() {
            if backup_path.exists() {
                std::fs::remove_file(&backup_path).with_context(|| {
                    format!("failed to remove old backup {}", backup_path.display())
                })?;
            }
            std::fs::copy(&self.path, &backup_path).with_context(|| {
                format!(
                    "failed to backup state from {} to {}",
                    self.path.display(),
                    backup_path.display()
                )
            })?;
        }

        let data = serde_json::to_vec_pretty(&self.state).context("failed to encode scan state")?;
        std::fs::write(&tmp_path, data)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.path)
            .with_context(|| format!("failed to atomically replace {}", self.path.display()))?;
        Ok(())
    }
}
