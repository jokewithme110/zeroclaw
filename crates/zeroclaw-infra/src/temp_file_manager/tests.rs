use crate::cleanup_rule::is_protected_path;
use crate::strategy::{
    CleanupStrategy, FileMeta, SpaceBasedStrategy, StrategyConfig, TimeBasedStrategy,
};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[test]
fn test_time_strategy_finds_expired() {
    let strategy = TimeBasedStrategy;
    let now = SystemTime::now();
    let old_file = FileMeta {
        path: PathBuf::from("/tmp/old.txt"),
        mtime: now - Duration::from_secs(3600 * 25), // 25 hours ago
        size_bytes: 1024,
    };
    let new_file = FileMeta {
        path: PathBuf::from("/tmp/new.txt"),
        mtime: now - Duration::from_secs(3600), // 1 hour ago
        size_bytes: 1024,
    };

    let config = StrategyConfig {
        retention_hours: 24,
        max_size_bytes: 0,
        current_dir_size_bytes: 0,
    };

    let to_delete = strategy.find_files_to_delete(&[old_file, new_file], &config);
    assert_eq!(to_delete.len(), 1);
    assert_eq!(to_delete[0].file_name().unwrap(), "old.txt");
}

#[test]
fn test_time_strategy_skips_recent() {
    let strategy = TimeBasedStrategy;
    let now = SystemTime::now();
    let recent_file = FileMeta {
        path: PathBuf::from("/tmp/recent.txt"),
        mtime: now - Duration::from_secs(3600), // 1 hour ago
        size_bytes: 1024,
    };

    let config = StrategyConfig {
        retention_hours: 24,
        max_size_bytes: 0,
        current_dir_size_bytes: 0,
    };

    let to_delete = strategy.find_files_to_delete(&[recent_file], &config);
    assert!(to_delete.is_empty());
}

#[test]
fn test_space_strategy_prunes_oldest_first() {
    let strategy = SpaceBasedStrategy;
    let now = SystemTime::now();

    // Create files that sum to 15 MB
    let files = vec![
        FileMeta {
            path: PathBuf::from("/tmp/old.txt"),
            mtime: now - Duration::from_secs(3600 * 3), // 3 hours ago
            size_bytes: 5 * 1024 * 1024,                // 5 MB
        },
        FileMeta {
            path: PathBuf::from("/tmp/middle.txt"),
            mtime: now - Duration::from_secs(3600 * 2), // 2 hours ago
            size_bytes: 5 * 1024 * 1024,                // 5 MB
        },
        FileMeta {
            path: PathBuf::from("/tmp/new.txt"),
            mtime: now - Duration::from_secs(3600), // 1 hour ago
            size_bytes: 5 * 1024 * 1024,            // 5 MB
        },
    ];

    let config = StrategyConfig {
        retention_hours: 0,
        max_size_bytes: 10 * 1024 * 1024,         // Limit to 10 MB
        current_dir_size_bytes: 15 * 1024 * 1024, // Current size is 15 MB
    };

    let to_delete = strategy.find_files_to_delete(&files, &config);
    // Should delete the oldest file (5 MB) to get under 10 MB limit
    assert_eq!(to_delete.len(), 1);
    assert_eq!(to_delete[0].file_name().unwrap(), "old.txt");
}

#[test]
fn test_space_strategy_no_op_when_under_limit() {
    let strategy = SpaceBasedStrategy;
    let now = SystemTime::now();

    let files = vec![FileMeta {
        path: PathBuf::from("/tmp/file.txt"),
        mtime: now,
        size_bytes: 5 * 1024 * 1024, // 5 MB
    }];

    let config = StrategyConfig {
        retention_hours: 0,
        max_size_bytes: 10 * 1024 * 1024,        // Limit to 10 MB
        current_dir_size_bytes: 5 * 1024 * 1024, // Current size is 5 MB
    };

    let to_delete = strategy.find_files_to_delete(&files, &config);
    assert!(to_delete.is_empty());
}

#[test]
fn test_protected_dirs_are_rejected() {
    let workspace = PathBuf::from("/tmp/workspace");

    // 受保护的目录应该被拒绝
    let protected_paths = vec![
        "cron",
        "cron/",
        "memory",
        "sessions",
        "skills",
        "state",
        "cron/subdir",
        "memory/cache",
    ];

    for path in protected_paths {
        assert!(
            is_protected_path(&workspace, path),
            "Expected {} to be protected",
            path
        );
    }

    // 允许的临时目录不应该被保护
    let allowed_paths = vec![
        "qq_files",
        "wechat_files",
        "dingtalk_files",
        "lark_files",
        "media",
        "logs",
        "cache",
        "downloads",
        "qq_files/attachments",
        "media/node_snap_front",
    ];

    for path in allowed_paths {
        assert!(
            !is_protected_path(&workspace, path),
            "Expected {} to NOT be protected",
            path
        );
    }

    let sensitive_paths = vec!["../tmp", ".git", ".env", "/tmp/absolute"];
    for path in sensitive_paths {
        assert!(
            is_protected_path(&workspace, path),
            "Expected {} to be protected",
            path
        );
    }
}

#[test]
fn test_interval_conversion_minutes() {
    use crate::temp_file_manager::{TempFileConfig, TempFileManager};
    use std::path::PathBuf;

    // Test 0.1 hours = 6 minutes
    let config = TempFileConfig {
        enabled: true,
        temp_file_retention_hours: 24,
        temp_file_max_size_mb: 50,
        scheduled_cleanup_enabled: true,
        scheduled_cleanup_interval_hours: 0.1, // 6 minutes
        rules: vec![],
    };

    let manager = TempFileManager::from_config(PathBuf::from("/tmp/test"), &config).unwrap();
    let duration = manager.calculate_interval_duration();

    // 0.1 hours = 6 minutes = 360 seconds
    assert_eq!(duration.as_secs(), 360);
}

#[test]
fn test_interval_minimum_enforcement() {
    use crate::temp_file_manager::{TempFileConfig, TempFileManager};
    use std::path::PathBuf;

    // Test interval less than 1 minute should be adjusted to 1 minute
    let config = TempFileConfig {
        enabled: true,
        temp_file_retention_hours: 24,
        temp_file_max_size_mb: 50,
        scheduled_cleanup_enabled: true,
        scheduled_cleanup_interval_hours: 0.01, // 0.6 minutes (< 1 minute)
        rules: vec![],
    };

    let manager = TempFileManager::from_config(PathBuf::from("/tmp/test"), &config).unwrap();
    let duration = manager.calculate_interval_duration();

    // Should be adjusted to 1 minute = 60 seconds
    assert_eq!(duration.as_secs(), 60);
}

#[test]
fn trigger_matches_builtin_channel_files_dir() {
    use crate::temp_file_manager::{TempFileConfig, TempFileManager};

    // New channel following the `<id>_files/` convention must pick up
    // the universal retention/size limits without any explicit
    // `files_cleanup.rules` entry. `trigger_cleanup_by_path` returns
    // `Ok(())` whether or not a file needs deletion, so we exercise
    // the call path that derives the rule internally.
    let tmp = tempfile::tempdir().unwrap();
    let slack_dir = tmp.path().join("slack_files");
    std::fs::create_dir_all(&slack_dir).unwrap();
    let file_path = slack_dir.join("hello.txt");
    std::fs::write(&file_path, b"hi").unwrap();

    let config = TempFileConfig {
        enabled: true,
        temp_file_retention_hours: 24,
        temp_file_max_size_mb: 50,
        scheduled_cleanup_enabled: false,
        scheduled_cleanup_interval_hours: 1.0,
        rules: vec![],
    };
    TempFileManager::trigger_cleanup_by_path(tmp.path(), &file_path, &config, false).unwrap();

    // The matching manager built from the same config registers
    // zero rules: built-in channels are intentionally NOT registered
    // into `self.rules` (they're message-triggered only). The
    // trigger path above is what actually governs attachment cleanup
    // for the `slack_files/` channel.
    let manager = TempFileManager::from_config(tmp.path().to_path_buf(), &config).unwrap();
    assert_eq!(manager.rules_count(), 0);
}

#[test]
fn trigger_falls_back_to_first_workspace_dir() {
    use crate::temp_file_manager::{TempFileConfig, TempFileManager};

    // File in an arbitrary `attachments/telegram/...` subdir should
    // still be governed by the universal limits via the generic
    // fallback rule.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("attachments").join("telegram");
    std::fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("img.png");
    std::fs::write(&file_path, b"data").unwrap();

    let config = TempFileConfig {
        enabled: true,
        temp_file_retention_hours: 24,
        temp_file_max_size_mb: 50,
        scheduled_cleanup_enabled: false,
        scheduled_cleanup_interval_hours: 1.0,
        rules: vec![],
    };
    TempFileManager::trigger_cleanup_by_path(tmp.path(), &file_path, &config, false).unwrap();

    // No user rules were configured, so `from_config` registers
    // zero rules (builtin channels are message-triggered only).
    let manager = TempFileManager::from_config(tmp.path().to_path_buf(), &config).unwrap();
    assert_eq!(manager.rules_count(), 0);
}

#[test]
fn trigger_respects_explicit_custom_rule() {
    use crate::temp_file_manager::{TempCleanupRule, TempFileConfig, TempFileManager};

    // Explicit `files_cleanup.rules` entry applies when the caller
    // opts into the full-resolution path (e.g. the scheduled scan).
    // The channel-message path (`include_custom_rules = false`)
    // intentionally skips these rules — they target the global
    // `data_dir` workspace and are reserved for background cleanup.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("slack_files");
    std::fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("hello.txt");
    std::fs::write(&file_path, b"hi").unwrap();

    let config = TempFileConfig {
        enabled: true,
        temp_file_retention_hours: 24,
        temp_file_max_size_mb: 50,
        scheduled_cleanup_enabled: false,
        scheduled_cleanup_interval_hours: 1.0,
        rules: vec![TempCleanupRule {
            path: "slack_files/".to_string(),
            pattern: Some("hello*".to_string()),
            retention_hours: 1,
            max_size_mb: 1,
        }],
    };
    TempFileManager::trigger_cleanup_by_path(tmp.path(), &file_path, &config, true).unwrap();
}
