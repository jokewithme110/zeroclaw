//! Provider-error Fluent domain helpers.
//!
//! This file owns loading and fallback behavior for `errors.ftl`, so the main
//! i18n module can reuse the same error-domain logic through small call sites.

use std::collections::HashMap;

pub fn load_error_strings(
    locale: &str,
    load_from_disk: impl Fn(&str, &str) -> Option<String>,
    format_messages: impl Fn(&str, &str) -> HashMap<String, String>,
) -> HashMap<String, String> {
    let mut map = format_messages(include_str!("../locales/en/errors.ftl"), "en");
    if locale != "en" {
        if let Some(locale_ftl) = builtin_error_ftl_source(locale) {
            map.extend(format_messages(locale_ftl, locale));
        }
        if let Some(locale_ftl) = load_from_disk(locale, "errors.ftl") {
            map.extend(format_messages(&locale_ftl, locale));
        }
    }
    map
}

pub fn missing_error_string(key: &str) -> String {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(
                ::serde_json::json!({"error_key": "i18n.missing_error_string", "key": key})
            ),
        "missing error Fluent string"
    );
    format!("{{{key}}}")
}

fn builtin_error_ftl_source(locale: &str) -> Option<&'static str> {
    match locale {
        "zh-CN" => Some(include_str!("../locales/zh-CN/errors.ftl")),
        _ => None,
    }
}
