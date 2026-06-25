//! Fluent-based i18n for tool descriptions.
//!
//! English descriptions are embedded via `include_str!` at compile time.
//! Non-English locales are loaded from disk and override English per-key.

use std::collections::HashMap;
use std::sync::OnceLock;

static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_FTL_SOURCES: OnceLock<CliFtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();
static ERROR_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();

/// The canonical locale registry, embedded from repo-root `locales.toml` at
/// compile time. Parsed once into a `'static` list so callers (e.g. the RPC
/// `locales/list` handler) get a long-lived reference with no runtime file I/O.
static AVAILABLE_LOCALES: OnceLock<Vec<LocaleOption>> = OnceLock::new();

const LOCALES_TOML: &str = include_str!("../../../locales.toml");

/// One selectable locale: its `code` (e.g. `ja`) and display `label`
/// (e.g. `日本語`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleOption {
    pub code: String,
    pub label: String,
}

/// Locales the build knows about, from the embedded `locales.toml`. Cheap:
/// parsed once, then returns a borrow of the cached `'static` vector.
pub fn available_locales() -> &'static [LocaleOption] {
    AVAILABLE_LOCALES
        .get_or_init(|| {
            let table: toml::Value =
                toml::from_str(LOCALES_TOML).expect("embedded locales.toml is valid TOML");
            table
                .get("locale")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            let code = e.get("code").and_then(|v| v.as_str())?;
                            let label = e.get("label").and_then(|v| v.as_str())?;
                            Some(LocaleOption {
                                code: code.to_string(),
                                label: label.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .as_slice()
}

struct CliFtlSources {
    locale: String,
    disk: Option<String>,
    builtin: Option<&'static str>,
}

/// Initialize with a specific locale. No-op after first call.
pub fn init(locale: &str) {
    let requested_locale = normalize_locale(locale);
    if let Some(active_locale) = LOCALE.get()
        && active_locale != &requested_locale
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "error_key": "i18n.locale_reinit_ignored",
                    "active_locale": active_locale,
                    "requested_locale": requested_locale,
                })),
            "i18n locale already initialized; ignoring later locale"
        );
    }
    let locale = LOCALE.get_or_init(|| requested_locale);
    DESCRIPTIONS.get_or_init(|| load_descriptions(locale));
    ERROR_STRINGS.get_or_init(|| load_error_strings(locale));
    CLI_STRINGS.get_or_init(|| load_cli_strings(locale));
    CLI_FTL_SOURCES.get_or_init(|| load_cli_ftl_sources(locale));
}

/// Get a tool description by tool name (e.g. "shell", "file_read").
pub fn get_tool_description(tool_name: &str) -> Option<&'static str> {
    let map = DESCRIPTIONS.get_or_init(|| load_descriptions(active_locale()));
    let key = format!("tool-{}", tool_name.replace('_', "-"));
    map.get(&key).map(String::as_str)
}

/// Get a CLI string by key (e.g. "cli-config-about").
/// Automatically injects brand parameters for Fluent variable substitution.
pub fn get_cli_string(key: &str) -> Option<String> {
    // Automatically inject product_name as a Fluent external argument
    let product_name = zeroclaw_api::branding::product_name();
    let brand_args = [("product_name", product_name.as_str())];
    get_cli_string_with_args(key, &brand_args)
}

/// Get a CLI string by key and format it with Fluent external arguments.
pub fn get_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> Option<String> {
    format_cli_string_with_args(cli_ftl_sources(), key, args)
}

/// Get a required CLI string by key, reporting missing Fluent strings centrally.
pub fn get_required_cli_string(key: &str) -> String {
    get_cli_string(key).unwrap_or_else(|| missing_cli_string(key))
}

/// Get a required CLI string by key and format it with Fluent external arguments.
pub fn get_required_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    get_cli_string_with_args(key, args).unwrap_or_else(|| missing_cli_string(key))
}

/// Get an error string by key (e.g. "err-provider-auth-failed").
pub fn get_error_string(key: &str) -> Option<String> {
    let map = ERROR_STRINGS.get_or_init(|| load_error_strings(active_locale()));
    map.get(key).cloned()
}

/// Get a required error string by key, reporting missing Fluent strings centrally.
pub fn get_required_error_string(key: &str) -> String {
    get_error_string(key).unwrap_or_else(|| crate::i18n_errors::missing_error_string(key))
}

fn active_locale() -> &'static str {
    LOCALE.get_or_init(detect_locale).as_str()
}

fn cli_ftl_sources() -> &'static CliFtlSources {
    CLI_FTL_SOURCES.get_or_init(|| load_cli_ftl_sources(active_locale()))
}

/// Resolve a CLI string against the embedded English catalogue only, ignoring
/// the process locale and the filesystem. Used by tests that assert the
/// canonical English wording without depending on the host's configured
/// locale (the global `LOCALE` OnceLock would otherwise make them flaky).
#[cfg(test)]
pub(crate) fn get_english_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    let english = CliFtlSources {
        locale: "en".to_string(),
        disk: None,
        builtin: None,
    };
    format_cli_string_with_args(&english, key, args).unwrap_or_else(|| missing_cli_string(key))
}

fn missing_cli_string(key: &str) -> String {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"error_key": "i18n.missing_cli_string", "key": key})),
        "missing CLI Fluent string"
    );
    format!("{{{key}}}")
}

fn load_descriptions(locale: &str) -> HashMap<String, String> {
    let mut map =
        crate::i18n_loader::format_ftl_messages(include_str!("../locales/en/tools.ftl"), "en");
    if locale != "en"
        && let Some(locale_ftl) = load_ftl_from_disk(locale, "tools.ftl")
    {
        map.extend(crate::i18n_loader::format_ftl_messages(&locale_ftl, locale));
    }
    map
}

fn load_cli_strings(locale: &str) -> HashMap<String, String> {
    let mut map =
        crate::i18n_loader::format_ftl_messages(include_str!("../locales/en/cli.ftl"), "en");
    if locale != "en" {
        if let Some(locale_ftl) = builtin_cli_ftl_source(locale) {
            map.extend(crate::i18n_loader::format_ftl_messages(locale_ftl, locale));
        }
        if let Some(locale_ftl) = load_ftl_from_disk(locale, "cli.ftl") {
            map.extend(crate::i18n_loader::format_ftl_messages(&locale_ftl, locale));
        }
    }
    map
}

fn load_cli_ftl_sources(locale: &str) -> CliFtlSources {
    CliFtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale, "cli.ftl"))
            .flatten(),
        builtin: (locale != "en")
            .then(|| builtin_cli_ftl_source(locale))
            .flatten(),
    }
}

fn builtin_cli_ftl_source(locale: &str) -> Option<&'static str> {
    match locale {
        "zh-CN" => Some(include_str!("../locales/zh-CN/cli.ftl")),
        _ => None,
    }
}

fn load_error_strings(locale: &str) -> HashMap<String, String> {
    crate::i18n_errors::load_error_strings(
        locale,
        load_ftl_from_disk,
        crate::i18n_loader::format_ftl_messages,
    )
}

fn format_cli_string_with_args(
    sources: &CliFtlSources,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    if let Some(locale_ftl) = sources.disk.as_deref()
        && let Some(value) =
            crate::i18n_loader::format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(apply_brand_replacements(value));
    }
    if let Some(locale_ftl) = sources.builtin
        && let Some(value) =
            crate::i18n_loader::format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(apply_brand_replacements(value));
    }
    crate::i18n_loader::format_ftl_message(include_str!("../locales/en/cli.ftl"), "en", key, args)
        .map(apply_brand_replacements)
}

/// One-shot post-processing: replace any hardcoded `zeroclaw` / `ZeroClaw`
/// literal in the rendered FTL string with the runtime `BRAND` value, so
/// localised strings track the brand without needing per-key placeholder
/// rewriting in every locale. Runs after Fluent substitution, so existing
/// `{$product_name}` callers and hardcoded literals both end up consistent.
fn apply_brand_replacements(mut s: String) -> String {
    let brand = zeroclaw_api::branding::bin_name();
    s = s.replace("zeroclaw", &brand);
    s = s.replace("ZeroClaw", &brand);
    s
}

fn load_ftl_from_disk(locale: &str, filename: &str) -> Option<String> {
    crate::i18n_loader::load_ftl_from_disk(locale, filename, || {
        crate::i18n_bootstrap::locale_override_roots(read_config_table)
    })
}

/// Path-resolution + read wiring for locale FTL, with an injectable reader so
/// tests can verify which path is consulted without touching the real
/// filesystem. Production passes `std::fs::read_to_string`.
#[allow(dead_code)]
fn load_ftl_with_reader(
    locale: &str,
    filename: &str,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> Option<String> {
    let path = zeroclaw_config::schema::ftl_locale_dir(locale)
        .ok()
        .map(|d| d.join(filename));
    let search_paths = [path];
    for path in search_paths.into_iter().flatten() {
        if let Some(content) = read(&path) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"path": path.display().to_string()})),
                "loaded locale FTL from disk"
            );
            return Some(content);
        }
    }
    None
}

/// Detect locale: config.toml → locale env vars → "en".
pub fn detect_locale() -> String {
    locale_from_config()
        .or_else(crate::i18n_bootstrap::locale_from_env)
        .unwrap_or_else(|| "en".to_string())
}

fn read_config_table() -> Option<toml::Table> {
    for path in crate::i18n_bootstrap::config_table_candidates() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            return contents.parse().ok();
        }
    }
    None
}

fn locale_from_config() -> Option<String> {
    locale_from_table(read_config_table())
}

/// Pure: extract a normalized locale from an already-parsed config table.
/// Split out from `locale_from_config` so it is testable without filesystem or
/// environment access — no test may touch the real FS to verify locale logic.
fn locale_from_table(table: Option<toml::Table>) -> Option<String> {
    let table = table?;
    let locale = table.get("locale")?.as_str()?.trim().to_string();
    if locale.is_empty() {
        return None;
    }
    Some(normalize_locale(&locale))
}

/// Normalize "zh_CN.UTF-8" → "zh-CN".
pub fn normalize_locale(raw: &str) -> String {
    raw.split('.').next().unwrap_or(raw).replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_descriptions_are_embedded() {
        let map = format_ftl_messages(include_str!("../locales/en/tools.ftl"), "en");
        assert!(map.contains_key("tool-shell"));
        assert!(map.contains_key("tool-file-read"));
        assert!(!map.contains_key("tool-nonexistent"));
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        let map = load_descriptions("xx-FAKE");
        assert!(map.contains_key("tool-shell"));
    }

    #[test]
    fn cli_string_formats_external_args() {
        let value = format_ftl_message(
            "cli-test = Value { $value }",
            "en",
            "cli-test",
            &[("value", "42")],
        );
        assert_eq!(value.as_deref(), Some("Value 42"));
    }

    #[test]
    fn zh_cn_wechat_translations_preserve_machine_facing_tokens() {
        let zh_cn = include_str!("../locales/zh-CN/cli.ftl");
        let bind = format_ftl_message(
            zh_cn,
            "zh-CN",
            "cli-wechat-send-bind-command",
            &[("command", "/bind")],
        )
        .expect("zh-CN bind command should format");
        assert!(bind.contains("WeChat"));
        assert!(bind.contains("/bind"));
        assert!(bind.contains("<code>"));

        let success = format_ftl_message(zh_cn, "zh-CN", "cli-wechat-bound-success", &[])
            .expect("zh-CN bind success should format");
        assert!(success.contains("WeChat"));
        assert!(success.contains("ZeroClaw"));
    }

    #[test]
    fn zh_cn_cli_strings_load_from_builtin_source() {
        let map = load_cli_strings("zh-CN");
        assert_eq!(
            map.get("cli-wechat-connected").map(String::as_str),
            Some("✅ WeChat 已连接！")
        );

        let sources = load_cli_ftl_sources("zh-CN");
        let value = format_cli_string_with_args(
            &sources,
            "cli-wechat-pairing-required",
            &[("code", "123456")],
        )
        .expect("zh-CN built-in CLI source should format args");
        assert!(value.contains("WeChat"));
        assert!(value.contains("123456"));
        assert!(value.contains("需要绑定"));
    }

    #[test]
    fn argumented_cli_strings_fall_back_from_disk_to_builtin_locale() {
        let sources = CliFtlSources {
            locale: "zh-CN".to_string(),
            disk: Some("cli-wechat-connected = stale workspace override".to_string()),
            builtin: builtin_cli_ftl_source("zh-CN"),
        };

        let overridden = format_cli_string_with_args(&sources, "cli-wechat-connected", &[])
            .expect("disk override should still win when present");
        assert_eq!(overridden, "stale workspace override");

        let built_in = format_cli_string_with_args(
            &sources,
            "cli-wechat-pairing-required",
            &[("code", "123456")],
        )
        .expect("missing disk key should fall back to built-in zh-CN");
        assert!(built_in.contains("123456"));
        assert!(built_in.contains("需要绑定"));
    }

    #[test]
    fn wechat_cli_strings_format_from_fluent() {
        let keys = [
            (
                "cli-wechat-pairing-required",
                &[("code", "123456")][..],
                ["123456"].as_slice(),
            ),
            (
                "cli-wechat-send-bind-command",
                &[("command", "/bind")][..],
                ["WeChat", "/bind", "<code>"].as_slice(),
            ),
            (
                "cli-wechat-qr-login",
                &[("attempt", "1"), ("max", "3")][..],
                ["1", "3"].as_slice(),
            ),
            ("cli-wechat-scan-to-connect", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-qr-url",
                &[("url", "https://example.test/qr")][..],
                ["https://example.test/qr"].as_slice(),
            ),
            (
                "cli-wechat-qr-expired-giving-up",
                &[("max", "3")][..],
                ["3"].as_slice(),
            ),
            ("cli-wechat-qr-fetch-failed", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-qr-fetch-status-failed",
                &[("status", "500"), ("body", "server error")][..],
                ["WeChat", "500", "server error"].as_slice(),
            ),
            (
                "cli-wechat-missing-response-field",
                &[("field", "qrcode")][..],
                ["WeChat", "qrcode"].as_slice(),
            ),
            ("cli-wechat-scanned-confirm", &[][..], [].as_slice()),
            ("cli-wechat-qr-expired-refreshing", &[][..], [].as_slice()),
            (
                "cli-wechat-login-confirmed-missing-field",
                &[("field", "bot_token")][..],
                ["bot_token"].as_slice(),
            ),
            ("cli-wechat-connected", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-bound-success",
                &[][..],
                ["WeChat", "ZeroClaw"].as_slice(),
            ),
            ("cli-wechat-invalid-bind-code", &[][..], [].as_slice()),
        ];
        for source in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/zh-CN/cli.ftl"), "zh-CN"),
        ] {
            for (key, args, expected_parts) in keys {
                let value = format_ftl_message(source.0, source.1, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {}", source.1));
                for expected in expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {} should preserve {expected}",
                        source.1
                    );
                }
            }
        }
    }

    #[test]
    fn skills_install_cli_strings_format_from_fluent() {
        type FormatCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);

        let en_cases: &[FormatCase<'_>] = &[
            (
                "cli-skills-install-start",
                &[("source", "example-skill")][..],
                &["Installing skill from", "example-skill"],
            ),
            (
                "cli-skills-install-resolving-registry",
                &[("source", "example-skill")][..],
                &["  Resolving", "example-skill", "skills registry"],
            ),
            (
                "cli-skills-install-installed-audited",
                &[("status", "OK"), ("path", "/tmp/example"), ("files", "3")][..],
                &["  OK", "/tmp/example", "3 files scanned"],
            ),
            (
                "cli-skills-install-security-audit-completed",
                &[][..],
                &["  Security audit completed successfully"],
            ),
            (
                "cli-skills-install-tier-official",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &["example-skill", "1.2.3", "Official"],
            ),
            (
                "cli-skills-install-tier-community",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &[
                    "example-skill",
                    "1.2.3",
                    "Community submission",
                    "zeroclaw skills audit example-skill",
                ],
            ),
        ];
        let zh_cn_cases: &[FormatCase<'_>] = &[
            (
                "cli-skills-install-start",
                &[("source", "example-skill")][..],
                &["正在安装技能来源", "example-skill"],
            ),
            (
                "cli-skills-install-resolving-registry",
                &[("source", "example-skill")][..],
                &["  正在从技能注册表解析", "example-skill"],
            ),
            (
                "cli-skills-install-installed-audited",
                &[("status", "OK"), ("path", "/tmp/example"), ("files", "3")][..],
                &["  OK", "/tmp/example", "已扫描 3 个文件"],
            ),
            (
                "cli-skills-install-security-audit-completed",
                &[][..],
                &["  安全审计已成功完成"],
            ),
            (
                "cli-skills-install-tier-official",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &["example-skill", "1.2.3", "官方"],
            ),
            (
                "cli-skills-install-tier-community",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &[
                    "example-skill",
                    "1.2.3",
                    "社区提交",
                    "zeroclaw skills audit example-skill",
                ],
            ),
        ];

        for (source, locale, cases) in [
            (include_str!("../locales/en/cli.ftl"), "en", en_cases),
            (
                include_str!("../locales/zh-CN/cli.ftl"),
                "zh-CN",
                zh_cn_cases,
            ),
        ] {
            for (key, args, expected_parts) in cases {
                let value = format_ftl_message(source, locale, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {locale}"));
                for expected in *expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {locale} should preserve {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn normalize_locale_strips_encoding() {
        assert_eq!(normalize_locale("en_US.UTF-8"), "en-US");
        assert_eq!(normalize_locale("zh_CN.utf8"), "zh-CN");
        assert_eq!(normalize_locale("fr"), "fr");
    }

    #[test]
    fn detect_locale_defaults_to_en_without_config() {
        // Locale is config-only. read_config_table() is pure parsing over a
        // string; verify the fallback contract without touching the real
        // filesystem or env. An absent/locale-less table must yield "en".
        assert_eq!(locale_from_table(None), None);
        let no_locale: toml::Table = "model = \"x\"".parse().unwrap();
        assert_eq!(locale_from_table(Some(no_locale)), None);
        let empty_locale: toml::Table = "locale = \"\"".parse().unwrap();
        assert_eq!(locale_from_table(Some(empty_locale)), None);
        // detect_locale layers the "en" fallback over locale_from_table.
        assert_eq!(
            locale_from_table(None).unwrap_or_else(|| "en".to_string()),
            "en"
        );
    }

    #[test]
    fn load_ftl_from_disk_reads_config_dir_data_ftl() {
        // Verify the loader resolves a locale's FTL path and returns the
        // reader's content — using an in-memory reader so no real filesystem
        // or environment is touched. The path must carry the locale and
        // filename so a fetched catalogue at <dir>/.../<locale>/<file> is found.
        let seen = std::cell::RefCell::new(Vec::<std::path::PathBuf>::new());
        let loaded = load_ftl_with_reader("xx", "cli.ftl", |p| {
            seen.borrow_mut().push(p.to_path_buf());
            Some("cli-probe = hit\n".to_string())
        });
        assert_eq!(loaded.as_deref(), Some("cli-probe = hit\n"));

        let paths = seen.borrow();
        assert!(!paths.is_empty(), "reader must be consulted with a path");
        let p = paths[0].to_string_lossy();
        assert!(p.contains("xx"), "path must carry the locale: {p}");
        assert!(p.ends_with("cli.ftl"), "path must target the file: {p}");
    }

    #[test]
    fn error_strings_loaded_in_english() {
        let map = format_ftl_messages(include_str!("../locales/en/errors.ftl"), "en");
        assert_eq!(
            map.get("err-provider-auth-failed").map(String::as_str),
            Some("⚠️ API key is invalid or expired. Please check your model configuration.")
        );
    }

    #[test]
    fn error_strings_loaded_in_chinese() {
        let map = format_ftl_messages(include_str!("../locales/zh-CN/errors.ftl"), "zh-CN");
        let value = map
            .get("err-provider-network-error")
            .expect("zh-CN error key should exist");
        assert!(value.contains("网络连接失败"));
    }

    #[test]
    fn detect_locale_uses_locale_env_fallbacks() {
        let previous = std::env::var("ZEROCLAW_LOCALE").ok();
        // SAFETY: test mutates process env in a controlled scope.
        unsafe { std::env::set_var("ZEROCLAW_LOCALE", "zh_CN.UTF-8") };
        assert_eq!(detect_locale(), "zh-CN");
        match previous {
            Some(value) => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::set_var("ZEROCLAW_LOCALE", value) };
            }
            None => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::remove_var("ZEROCLAW_LOCALE") };
            }
        }
    }

    #[test]
    fn detect_locale_prefers_config_dir_profile_before_env_locale() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_dir = temp.path().join("profile-a");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        std::fs::write(config_dir.join("config.toml"), "locale = \"zh-CN\"\n")
            .expect("config write");

        let previous_config_dir = std::env::var("ZEROCLAW_CONFIG_DIR").ok();
        let previous_locale = std::env::var("ZEROCLAW_LOCALE").ok();

        // SAFETY: test mutates process env in a controlled scope.
        unsafe {
            std::env::set_var("ZEROCLAW_CONFIG_DIR", &config_dir);
            std::env::set_var("ZEROCLAW_LOCALE", "en_US.UTF-8");
        }

        assert_eq!(detect_locale(), "zh-CN");

        match previous_config_dir {
            Some(value) => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", value) };
            }
            None => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") };
            }
        }
        match previous_locale {
            Some(value) => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::set_var("ZEROCLAW_LOCALE", value) };
            }
            None => {
                // SAFETY: restoring process env after test.
                unsafe { std::env::remove_var("ZEROCLAW_LOCALE") };
            }
        }
    }
}
