//! Channel-facing provider error rendering helpers.
//!
//! This file decides when an orchestrator error should be converted into a
//! localized provider-friendly message for end users, keeping the main
//! orchestrator module focused on control flow instead of presentation rules.

use anyhow::Error;

pub struct ProviderErrorRenderContext<'a> {
    pub provider: &'a str,
    pub model: &'a str,
}

pub fn provider_error_user_message(
    error: &Error,
    render_ctx: &ProviderErrorRenderContext<'_>,
) -> String {
    let provider = provider_display_name(render_ctx.provider);
    let args = [("provider", provider), ("model", render_ctx.model)];

    let message = error.to_string();
    if message.contains("All model_providers/models failed. Attempts:") {
        let attempt_message = message
            .lines()
            .rev()
            .find_map(|line| line.split("error=").nth(1))
            .map(str::trim)
            .unwrap_or(message.as_str());
        return zeroclaw_runtime::i18n::get_required_error_string_with_args(
            zeroclaw_providers::provider_error::classify_provider_error_message(attempt_message)
                .error_key(),
            &args,
        );
    }

    zeroclaw_runtime::i18n::get_required_error_string_with_args(
        zeroclaw_providers::provider_error::classify_provider_error(error).error_key(),
        &args,
    )
}

fn provider_display_name(provider_ref: &str) -> &str {
    zeroclaw_config::providers::split_provider_ref(provider_ref)
        .map(|(provider_type, _alias)| provider_type)
        .unwrap_or_else(|| provider_ref.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_renders_parameterized_message() {
        let err = anyhow::Error::msg("LLM inference step timed out after 30s");
        let render_ctx = ProviderErrorRenderContext {
            provider: "DeepSeek",
            model: "deepseek-chat",
        };

        let rendered = provider_error_user_message(&err, &render_ctx);

        assert!(rendered.contains("timed out") || rendered.contains("超时"));
        assert!(!rendered.contains("30s"));
    }

    #[test]
    fn unknown_error_uses_friendly_fallback() {
        let err = anyhow::Error::msg("raw internal stack trace details should not leak");
        let render_ctx = ProviderErrorRenderContext {
            provider: "DeepSeek",
            model: "deepseek-chat",
        };

        let rendered = provider_error_user_message(&err, &render_ctx);

        assert!(!rendered.contains("stack trace"));
        assert!(rendered.contains("Service error") || rendered.contains("服务异常"));
    }

    #[test]
    fn provider_ref_alias_is_not_shown_to_users() {
        let err = anyhow::Error::msg("429: insufficient_quota, please check billing");
        let render_ctx = ProviderErrorRenderContext {
            provider: "deepseek.default",
            model: "deepseek-v4-flash",
        };

        let rendered = provider_error_user_message(&err, &render_ctx);

        assert!(rendered.contains("deepseek"));
        assert!(!rendered.contains("deepseek.default"));
    }
}
