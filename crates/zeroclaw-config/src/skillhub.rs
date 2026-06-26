//! SkillHub integration helpers.
//!
//! The SkillHub is a registry that exposes `search`, `list`, `detail`, and
//! `download` endpoints over HTTPS. ZeroClaw reads the base URL from
//! `SkillsConfig::skillhub_base_url` and resolves the effective value at
//! use-time (no caching into runtime structs to keep the config as the
//! single source of truth).

use crate::schema::Config;

/// Default SkillHub base URL used when no override is configured.
///
/// This matches the legacy hard-coded `CLAWHUB_DOWNLOAD_API` constant so
/// existing setups continue to work without any configuration change.
pub const DEFAULT_SKILLHUB_BASE_URL: &str = "https://clawhub.ai";

/// Resolve the effective SkillHub base URL.
///
/// Priority: explicit `skills.skillhub_base_url` config > `DEFAULT_SKILLHUB_BASE_URL`.
///
/// The returned string has **no trailing slash**, so callers can safely do
/// `format!("{base}/api/v1/skills")` without producing `//`.
///
/// **Hot-path note**: returns a freshly allocated `String` on every call.
/// If called per tool execution, consider caching the result in the
/// tool's constructor as `Arc<String>`.
pub fn resolve_skillhub_base_url(config: &Config) -> String {
    if let Some(url) = config.skills.skillhub_base_url.as_deref() {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_SKILLHUB_BASE_URL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(url: Option<&str>) -> Config {
        let mut c = Config::default();
        c.skills.skillhub_base_url = url.map(str::to_string);
        c
    }

    #[test]
    fn explicit_config_value_is_returned() {
        let c = cfg_with(Some("https://skillhubictst.mec189.cn/enhance"));
        assert_eq!(
            resolve_skillhub_base_url(&c),
            "https://skillhubictst.mec189.cn/enhance"
        );
    }

    #[test]
    fn trailing_slash_is_stripped() {
        let c = cfg_with(Some("https://skillhubictst.mec189.cn/enhance/"));
        assert_eq!(
            resolve_skillhub_base_url(&c),
            "https://skillhubictst.mec189.cn/enhance"
        );
        // also handle multiple trailing slashes
        let c = cfg_with(Some("https://example.com///"));
        assert_eq!(resolve_skillhub_base_url(&c), "https://example.com");
    }

    #[test]
    fn default_is_used_when_config_is_none() {
        let c = cfg_with(None);
        assert_eq!(resolve_skillhub_base_url(&c), DEFAULT_SKILLHUB_BASE_URL);
    }

    #[test]
    fn default_is_used_when_config_is_empty_or_whitespace() {
        let c = cfg_with(Some(""));
        assert_eq!(resolve_skillhub_base_url(&c), DEFAULT_SKILLHUB_BASE_URL);
        let c = cfg_with(Some("   "));
        assert_eq!(resolve_skillhub_base_url(&c), DEFAULT_SKILLHUB_BASE_URL);
    }

    #[test]
    fn leading_and_trailing_whitespace_is_trimmed() {
        let c = cfg_with(Some("  https://example.com/path  "));
        assert_eq!(resolve_skillhub_base_url(&c), "https://example.com/path");
    }
}
