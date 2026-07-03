//! Data management CLI commands (temporary file cleanup).
//!
//! Provides commands for querying temporary file usage and manually
//! triggering cleanup operations.

use crate::DataManagementCommands;
use crate::config::Config;
use anyhow::{Context, Result};
use zeroclaw_infra::temp_file_manager::{
    TempCleanupRule as InfraRule, TempFileCategory, TempFileConfig,
};
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

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

    println!("{}", get_required_cli_string("cli-dm-status-title"));
    println!("============================\n");

    println!("{}", get_required_cli_string("cli-dm-config"));
    let enabled_str = tf_config.enabled.to_string();
    println!(
        "{}",
        get_required_cli_string_with_args("cli-dm-enabled", &[("value", &enabled_str)])
    );
    let scheduled_value = if tf_config.scheduled_cleanup_enabled {
        let hours = tf_config.scheduled_cleanup_interval_hours;
        let minutes = hours * 60.0;
        if minutes < 60.0 {
            get_required_cli_string_with_args(
                "cli-dm-scheduled-minutes",
                &[("minutes", &format!("{:.2}", minutes))],
            )
        } else {
            get_required_cli_string_with_args(
                "cli-dm-scheduled-hours",
                &[("hours", &format!("{:.1}", hours))],
            )
        }
    } else {
        get_required_cli_string("cli-dm-scheduled-disabled")
    };
    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-dm-scheduled-cleanup",
            &[("value", &scheduled_value)]
        )
    );
    let retention_str = tf_config.temp_file_retention_hours.to_string();
    println!(
        "{}",
        get_required_cli_string_with_args("cli-dm-retention", &[("value", &retention_str)])
    );
    let max_size_str = tf_config.temp_file_max_size_mb.to_string();
    println!(
        "{}",
        get_required_cli_string_with_args("cli-dm-max-size", &[("value", &max_size_str)])
    );
    let rules_count_str = tf_config.rules.len().to_string();
    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-dm-custom-rules-count",
            &[("count", &rules_count_str)]
        )
    );

    if !tf_config.rules.is_empty() {
        println!(
            "\n{}",
            get_required_cli_string("cli-dm-custom-rules-header")
        );
        for (i, rule) in tf_config.rules.iter().enumerate() {
            let pattern_str = format!("{:?}", rule.pattern);
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-dm-custom-rule-item",
                    &[
                        ("n", &(i + 1).to_string()),
                        ("path", rule.path.as_str()),
                        ("pattern", &pattern_str),
                        ("retention", &rule.retention_hours.to_string()),
                        ("max_size", &rule.max_size_mb.to_string()),
                    ],
                )
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
            println!("\n{}", get_required_cli_string("cli-dm-usage-header"));

            // Query QQ attachments
            match manager.get_usage(&TempFileCategory::QqAttachments) {
                Ok(usage) => {
                    println!("\n{}", get_required_cli_string("cli-dm-qq-header"));
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-total-size",
                            &[("size", &format!("{:.2}", usage.total_size_mb))]
                        )
                    );
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-file-count",
                            &[("count", &usage.file_count.to_string())]
                        )
                    );
                    if usage.file_count > 0 {
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-oldest",
                                &[("age", &format!("{:.1}", usage.oldest_file_age_hours))]
                            )
                        );
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-newest",
                                &[("age", &format!("{:.1}", usage.newest_file_age_hours))]
                            )
                        );
                    }
                }
                Err(e) => {
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-qq-error",
                            &[("error", &e.to_string())]
                        )
                    );
                }
            }

            // Query Node camera snapshots
            match manager.get_usage(&TempFileCategory::NodeCameraSnaps) {
                Ok(usage) => {
                    println!("\n{}", get_required_cli_string("cli-dm-node-header"));
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-total-size",
                            &[("size", &format!("{:.2}", usage.total_size_mb))]
                        )
                    );
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-file-count",
                            &[("count", &usage.file_count.to_string())]
                        )
                    );
                    if usage.file_count > 0 {
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-oldest",
                                &[("age", &format!("{:.1}", usage.oldest_file_age_hours))]
                            )
                        );
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-newest",
                                &[("age", &format!("{:.1}", usage.newest_file_age_hours))]
                            )
                        );
                    }
                }
                Err(e) => {
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-node-error",
                            &[("error", &e.to_string())]
                        )
                    );
                }
            }

            // Query custom rule directories
            for rule in &tf_config.rules {
                let category =
                    TempFileCategory::Custom(rule.path.trim_end_matches('/').to_string());
                match manager.get_usage(&category) {
                    Ok(usage) => {
                        println!(
                            "\n{}",
                            get_required_cli_string_with_args(
                                "cli-dm-custom-rule-header",
                                &[("path", rule.path.as_str())]
                            )
                        );
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-total-size",
                                &[("size", &format!("{:.2}", usage.total_size_mb))]
                            )
                        );
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-file-count",
                                &[("count", &usage.file_count.to_string())]
                            )
                        );
                        if usage.file_count > 0 {
                            println!(
                                "{}",
                                get_required_cli_string_with_args(
                                    "cli-dm-oldest",
                                    &[("age", &format!("{:.1}", usage.oldest_file_age_hours))]
                                )
                            );
                            println!(
                                "{}",
                                get_required_cli_string_with_args(
                                    "cli-dm-newest",
                                    &[("age", &format!("{:.1}", usage.newest_file_age_hours))]
                                )
                            );
                        }
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-dm-custom-rule-error",
                                &[("path", rule.path.as_str()), ("error", &e.to_string()),],
                            )
                        );
                    }
                }
            }
        }
        Err(e) => {
            println!(
                "\n{}",
                get_required_cli_string_with_args(
                    "cli-dm-warn-manager",
                    &[("error", &e.to_string())]
                )
            );
        }
    }

    println!();
    Ok(())
}

/// Manually trigger temporary file cleanup
fn trigger_temp_cleanup(config: &Config) -> Result<()> {
    let tf_config = &config.files_cleanup;

    if !tf_config.enabled {
        println!("{}", get_required_cli_string("cli-dm-disabled"));
        println!("{}", get_required_cli_string("cli-dm-disabled-hint"));
        return Ok(());
    }

    println!("{}\n", get_required_cli_string("cli-dm-clean-start"));

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
        Ok(manager) => match manager.enforce_all() {
            Ok(report) => {
                println!("{}", get_required_cli_string("cli-dm-clean-done"));
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-dm-rules-executed",
                        &[("count", &report.rules_executed.to_string())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-dm-files-deleted",
                        &[("count", &report.files_deleted.to_string())]
                    )
                );

                if report.bytes_freed > 0 {
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-dm-space-freed",
                            &[(
                                "size",
                                &format!("{:.2}", report.bytes_freed as f64 / 1024.0 / 1024.0)
                            )]
                        )
                    );
                }

                if !report.errors.is_empty() {
                    println!("\n{}", get_required_cli_string("cli-dm-errors-header"));
                    for (rule_path, error_msg) in &report.errors {
                        println!("  - {}: {}", rule_path, error_msg);
                    }
                }
            }
            Err(e) => {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-dm-clean-failed",
                        &[("error", &e.to_string())]
                    )
                );
                return Err(e);
            }
        },
        Err(e) => {
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-dm-init-failed",
                    &[("error", &e.to_string())]
                )
            );
            return Err(e);
        }
    }

    println!();
    Ok(())
}
