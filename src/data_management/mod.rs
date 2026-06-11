//! Data management CLI commands (temporary file cleanup).
//!
//! Provides commands for querying temporary file usage and manually
//! triggering cleanup operations.

use crate::DataManagementCommands;
use crate::config::Config;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_infra::temp_file_manager::{
    TempCleanupRule as InfraRule, TempFileCategory, TempFileConfig,
};

/// Handle data management CLI commands
pub fn handle_command(cmd: DataManagementCommands, config: &Config) -> Result<()> {
    match cmd {
        DataManagementCommands::TempStatus => show_temp_status(config),
        DataManagementCommands::TempClean => trigger_temp_cleanup(config),
    }
}

/// Show temporary file usage status
fn show_temp_status(config: &Config) -> Result<()> {
    let tf_config = &config.files_cleanup;

    println!("Temporary File Cleanup Status");
    println!("============================\n");

    println!("Configuration:");
    println!("  Enabled: {}", tf_config.enabled);
    println!(
        "  Scheduled cleanup: {}",
        if tf_config.scheduled_cleanup_enabled {
            let hours = tf_config.scheduled_cleanup_interval_hours;
            let minutes = hours * 60.0;
            if minutes < 60.0 {
                format!("every {:.2} minutes", minutes)
            } else {
                format!("every {:.1} hours", hours)
            }
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "  Built-in retention: {} hours",
        tf_config.temp_file_retention_hours
    );
    println!(
        "  Built-in max size: {} MB",
        tf_config.temp_file_max_size_mb
    );
    println!("  Custom rules: {}", tf_config.rules.len());

    if !tf_config.rules.is_empty() {
        println!("\nCustom Rules:");
        for (i, rule) in tf_config.rules.iter().enumerate() {
            println!(
                "  {}. path={}, pattern={:?}, retention={}h, max_size={}MB",
                i + 1,
                rule.path,
                rule.pattern,
                rule.retention_hours,
                rule.max_size_mb
            );
        }
    }

    // Create temp file manager to query usage
    let infra_config = TempFileConfig {
        enabled: tf_config.enabled,
        temp_file_retention_hours: tf_config.temp_file_retention_hours,
        temp_file_max_size_mb: tf_config.temp_file_max_size_mb,
        scheduled_cleanup_enabled: tf_config.scheduled_cleanup_enabled,
        scheduled_cleanup_interval_hours: tf_config.scheduled_cleanup_interval_hours as f64,
        rules: tf_config
            .rules
            .iter()
            .map(|r| InfraRule {
                path: r.path.clone(),
                pattern: r.pattern.clone(),
                retention_hours: r.retention_hours,
                max_size_mb: r.max_size_mb,
            })
            .collect(),
    };

    match zeroclaw_infra::temp_file_manager::TempFileManager::from_config(
        config.data_dir.clone(),
        &infra_config,
    ) {
        Ok(manager) => {
            println!("\nUsage Information:");

            // Query QQ attachments
            match manager.get_usage(&TempFileCategory::QqAttachments) {
                Ok(usage) => {
                    println!("\n  QQ Attachments (qq_files/):");
                    println!("    Total size: {:.2} MB", usage.total_size_mb);
                    println!("    File count: {}", usage.file_count);
                    if usage.file_count > 0 {
                        println!(
                            "    Oldest file: {:.1} hours old",
                            usage.oldest_file_age_hours
                        );
                        println!(
                            "    Newest file: {:.1} hours old",
                            usage.newest_file_age_hours
                        );
                    }
                }
                Err(e) => {
                    println!("  QQ Attachments: Error - {}", e);
                }
            }

            // Query Node camera snapshots
            match manager.get_usage(&TempFileCategory::NodeCameraSnaps) {
                Ok(usage) => {
                    println!("\n  Node Camera Snaps (media/node_snap_*):");
                    println!("    Total size: {:.2} MB", usage.total_size_mb);
                    println!("    File count: {}", usage.file_count);
                    if usage.file_count > 0 {
                        println!(
                            "    Oldest file: {:.1} hours old",
                            usage.oldest_file_age_hours
                        );
                        println!(
                            "    Newest file: {:.1} hours old",
                            usage.newest_file_age_hours
                        );
                    }
                }
                Err(e) => {
                    println!("  Node Camera Snaps: Error - {}", e);
                }
            }

            // Query custom rule directories
            for rule in &tf_config.rules {
                let category =
                    TempFileCategory::Custom(rule.path.trim_end_matches('/').to_string());
                match manager.get_usage(&category) {
                    Ok(usage) => {
                        println!("\n  Custom Rule ({}):", rule.path);
                        println!("    Total size: {:.2} MB", usage.total_size_mb);
                        println!("    File count: {}", usage.file_count);
                        if usage.file_count > 0 {
                            println!(
                                "    Oldest file: {:.1} hours old",
                                usage.oldest_file_age_hours
                            );
                            println!(
                                "    Newest file: {:.1} hours old",
                                usage.newest_file_age_hours
                            );
                        }
                    }
                    Err(e) => {
                        println!("  Custom Rule ({}): Error - {}", rule.path, e);
                    }
                }
            }
        }
        Err(e) => {
            println!("\nWarning: Failed to create temp file manager: {}", e);
        }
    }

    println!();
    Ok(())
}

/// Manually trigger temporary file cleanup
fn trigger_temp_cleanup(config: &Config) -> Result<()> {
    let tf_config = &config.files_cleanup;

    if !tf_config.enabled {
        println!("Temporary file cleanup is disabled in configuration.");
        println!("Enable it by setting files_cleanup.enabled = true in config.toml");
        return Ok(());
    }

    println!("Starting manual temporary file cleanup...\n");

    let infra_config = TempFileConfig {
        enabled: tf_config.enabled,
        temp_file_retention_hours: tf_config.temp_file_retention_hours,
        temp_file_max_size_mb: tf_config.temp_file_max_size_mb,
        scheduled_cleanup_enabled: tf_config.scheduled_cleanup_enabled,
        scheduled_cleanup_interval_hours: tf_config.scheduled_cleanup_interval_hours as f64,
        rules: tf_config
            .rules
            .iter()
            .map(|r| InfraRule {
                path: r.path.clone(),
                pattern: r.pattern.clone(),
                retention_hours: r.retention_hours,
                max_size_mb: r.max_size_mb,
            })
            .collect(),
    };

    match zeroclaw_infra::temp_file_manager::TempFileManager::from_config(
        config.data_dir.clone(),
        &infra_config,
    ) {
        Ok(manager) => {
            match manager.enforce_all() {
                Ok(report) => {
                    println!("Cleanup completed:");
                    println!("  Rules executed: {}", report.rules_executed);
                    println!("  Files deleted: {}", report.files_deleted);

                    // Estimate bytes freed (simplified - actual implementation would track this)
                    if report.bytes_freed > 0 {
                        println!(
                            "  Space freed: {:.2} MB",
                            report.bytes_freed as f64 / 1024.0 / 1024.0
                        );
                    }

                    if !report.errors.is_empty() {
                        println!("\nErrors encountered:");
                        for (rule_path, error_msg) in &report.errors {
                            println!("  - {}: {}", rule_path, error_msg);
                        }
                    }
                }
                Err(e) => {
                    println!("Cleanup failed: {}", e);
                    return Err(e);
                }
            }
        }
        Err(e) => {
            println!("Failed to initialize temp file manager: {}", e);
            return Err(e);
        }
    }

    println!();
    Ok(())
}
