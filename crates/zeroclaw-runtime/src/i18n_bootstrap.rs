//! Early i18n bootstrap path helpers.
//!
//! This file contains the config-dir, workspace-dir, and locale env resolution
//! helpers that must run before the main i18n OnceLock is initialized.

use std::path::PathBuf;

pub fn locale_from_env() -> Option<String> {
    for key in ["ZEROCLAW_LOCALE", "LC_ALL", "LANG"] {
        if let Ok(raw) = std::env::var(key) {
            let locale = raw.trim();
            if !locale.is_empty() {
                return Some(crate::i18n::normalize_locale(locale));
            }
        }
    }
    None
}

pub fn config_table_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let config_dir = config_dir.trim();
        if !config_dir.is_empty() {
            candidates.push(expand_tilde_path(config_dir).join("config.toml"));
        }
    }

    if let Ok(data_dir) = std::env::var("ZEROCLAW_DATA_DIR")
        && !data_dir.trim().is_empty()
    {
        let (config_dir, _) =
            zeroclaw_config::schema::resolve_config_dir_for_data(&expand_tilde_path(&data_dir));
        candidates.push(config_dir.join("config.toml"));
    }

    if let Ok(workspace_dir) = std::env::var("ZEROCLAW_WORKSPACE")
        && !workspace_dir.is_empty()
    {
        let (config_dir, _) = zeroclaw_config::schema::resolve_config_dir_for_data(
            &expand_tilde_path(&workspace_dir),
        );
        candidates.push(config_dir.join("config.toml"));
    }

    if let Some(default_dir) = default_config_dir() {
        candidates.push(default_dir.join("config.toml"));
    }

    if let Some(base) = directories::BaseDirs::new() {
        candidates.push(base.config_dir().join("zeroclaw/config.toml"));
    }

    candidates.dedup();
    candidates
}

pub fn locale_override_roots(read_config_table: impl Fn() -> Option<toml::Table>) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(data_dir) = std::env::var("ZEROCLAW_DATA_DIR")
        && !data_dir.trim().is_empty()
    {
        let (_, data_dir) =
            zeroclaw_config::schema::resolve_config_dir_for_data(&expand_tilde_path(&data_dir));
        roots.push(data_dir);
    }

    if let Ok(workspace_dir) = std::env::var("ZEROCLAW_WORKSPACE")
        && !workspace_dir.is_empty()
    {
        let (_, data_dir) = zeroclaw_config::schema::resolve_config_dir_for_data(
            &expand_tilde_path(&workspace_dir),
        );
        roots.push(data_dir);
    }

    if let Some(config_path) = config_table_candidates()
        .into_iter()
        .find(|path| path.exists())
        && let Some(install_root) = config_path.parent()
    {
        roots.push(install_root.join("shared"));
        roots.push(install_root.join("workspace"));
    } else if let Some(default_dir) = default_config_dir() {
        roots.push(default_dir.join("workspace"));
    }

    if let Some(dir) = read_config_table()
        .as_ref()
        .and_then(|t| t.get("workspace_dir"))
        .and_then(|v| v.as_str())
    {
        roots.push(PathBuf::from(dir));
    }

    roots.dedup();
    roots
}

fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home));
    }
    directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~')
        && let Some(home) = home_dir()
    {
        return home.join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(path)
}

fn default_config_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".zeroclaw"))
}
