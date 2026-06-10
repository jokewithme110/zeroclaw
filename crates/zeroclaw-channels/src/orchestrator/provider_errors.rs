//! Channel-facing provider error rendering helpers.
//!
//! This file decides when an orchestrator error should be converted into a
//! localized provider-friendly message for end users, keeping the main
//! orchestrator module focused on control flow instead of presentation rules.

use anyhow::Error;

pub fn provider_error_user_message(error: &Error) -> Option<String> {
    if error
        .downcast_ref::<zeroclaw_providers::ProviderCapabilityError>()
        .is_some()
    {
        return Some(zeroclaw_runtime::i18n::get_required_error_string(
            zeroclaw_providers::provider_error::classify_provider_error(error).error_key(),
        ));
    }

    let message = error.to_string();
    if message.contains("All model_providers/models failed. Attempts:") {
        let attempt_message = message
            .lines()
            .rev()
            .find_map(|line| line.split("error=").nth(1))
            .map(str::trim)
            .unwrap_or(message.as_str());
        return Some(zeroclaw_runtime::i18n::get_required_error_string(
            zeroclaw_providers::provider_error::classify_provider_error_message(attempt_message)
                .error_key(),
        ));
    }

    if zeroclaw_providers::provider_error::is_auth_error(error)
        || zeroclaw_providers::provider_error::is_rate_limited(error)
        || zeroclaw_providers::provider_error::is_non_retryable_rate_limit(error)
        || zeroclaw_providers::provider_error::is_model_not_found(error)
        || zeroclaw_providers::provider_error::is_network_error(error)
        || zeroclaw_providers::provider_error::is_server_error(error)
    {
        return Some(zeroclaw_runtime::i18n::get_required_error_string(
            zeroclaw_providers::provider_error::classify_provider_error(error).error_key(),
        ));
    }

    None
}
