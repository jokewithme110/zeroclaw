//! Provider failure classification helpers.
//!
//! This file centralizes the user-facing categorization of provider errors so
//! retry logic, channel output, and tests can all depend on one source of
//! truth without embedding the heuristics in larger runtime modules.

use anyhow::Error;

/// Error category for provider failures, used to produce user-friendly messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    AuthFailed,
    RateLimited,
    QuotaExceeded,
    ModelNotFound,
    VisionNotSupported,
    ContextWindowExceeded,
    Timeout,
    NetworkError,
    ServerError,
    Unknown,
}

impl ProviderErrorKind {
    pub fn error_key(self) -> &'static str {
        match self {
            Self::AuthFailed => "err-provider-auth-failed",
            Self::RateLimited => "err-provider-rate-limited",
            Self::QuotaExceeded => "err-provider-quota-exceeded",
            Self::ModelNotFound => "err-provider-model-not-found",
            Self::VisionNotSupported => "err-provider-vision-not-supported",
            Self::ContextWindowExceeded => "err-provider-context-window-exceeded",
            Self::Timeout => "err-provider-timeout",
            Self::NetworkError => "err-provider-network-error",
            Self::ServerError => "err-provider-server-error",
            Self::Unknown => "err-provider-unknown",
        }
    }
}

pub fn classify_provider_error(err: &Error) -> ProviderErrorKind {
    if let Some(cap) = err.downcast_ref::<zeroclaw_api::model_provider::ProviderCapabilityError>()
        && cap.capability.eq_ignore_ascii_case("vision")
    {
        return ProviderErrorKind::VisionNotSupported;
    }

    if is_auth_error(err) {
        return ProviderErrorKind::AuthFailed;
    }

    if is_non_retryable_rate_limit(err) || has_quota_business_hint(err) {
        return ProviderErrorKind::QuotaExceeded;
    }

    if is_rate_limited(err) {
        return ProviderErrorKind::RateLimited;
    }

    if is_context_window_exceeded(err) {
        return ProviderErrorKind::ContextWindowExceeded;
    }

    if is_model_not_found(err) {
        return ProviderErrorKind::ModelNotFound;
    }

    if is_server_error(err) {
        return ProviderErrorKind::ServerError;
    }

    if is_timeout_error(err) {
        return ProviderErrorKind::Timeout;
    }

    if is_network_error(err) {
        return ProviderErrorKind::NetworkError;
    }

    ProviderErrorKind::Unknown
}

pub fn classify_provider_error_message(message: &str) -> ProviderErrorKind {
    classify_provider_error(&Error::msg(message.to_string()))
}

pub fn is_auth_error(err: &Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        let code = status.as_u16();
        return code == 401 || code == 403;
    }

    let msg_lower = err.to_string().to_lowercase();
    let hints = [
        "401 unauthorized",
        "403 forbidden",
        "invalid api key",
        "incorrect api key",
        "authentication failed",
        "auth failed",
        "unauthorized",
        "invalid token",
        "token expired",
        "access_token",
    ];

    hints.iter().any(|hint| msg_lower.contains(hint))
}

pub fn is_rate_limited(err: &Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        return status.as_u16() == 429;
    }
    let msg = err.to_string();
    let lower = msg.to_lowercase();
    msg.split(|c: char| !c.is_ascii_digit())
        .any(|token| token == "429")
        && (lower.contains("too many")
            || lower.contains("rate")
            || lower.contains("limit")
            || lower.contains("retry-after")
            || lower.contains("retry_after"))
}

pub fn is_non_retryable_rate_limit(err: &Error) -> bool {
    if !is_rate_limited(err) {
        return false;
    }

    let msg = err.to_string();
    let lower = msg.to_lowercase();

    let business_hints = [
        "plan does not include",
        "doesn't include",
        "not include",
        "insufficient balance",
        "insufficient_balance",
        "insufficient quota",
        "insufficient_quota",
        "quota exhausted",
        "out of credits",
        "no available package",
        "package not active",
        "purchase package",
        "model not available for your plan",
    ];

    if business_hints.iter().any(|hint| lower.contains(hint)) {
        return true;
    }

    for token in lower.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = token.parse::<u16>()
            && matches!(code, 1113 | 1311)
        {
            return true;
        }
    }

    false
}

pub fn is_network_error(err: &Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && (reqwest_err.is_connect() || reqwest_err.is_request())
    {
        return true;
    }
    let lower = err.to_string().to_lowercase();
    let hints = [
        "connection reset",
        "connection refused",
        "dns error",
        "failed to resolve",
        "unreachable",
        "tcp connect error",
        "connection closed",
        "broken pipe",
        "error sending request",
    ];
    hints.iter().any(|hint| lower.contains(hint))
}

pub fn is_timeout_error(err: &Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && reqwest_err.is_timeout()
    {
        return true;
    }

    let lower = err.to_string().to_lowercase();
    let hints = [
        "timed out",
        "timeout",
        "deadline exceeded",
        "request timeout",
        "operation timeout",
    ];
    hints.iter().any(|hint| lower.contains(hint))
}

pub fn is_server_error(err: &Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        return status.is_server_error();
    }
    let msg = err.to_string();
    for word in msg.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = word.parse::<u16>()
            && (500..600).contains(&code)
        {
            return true;
        }
    }
    let lower = msg.to_lowercase();
    let hints = [
        "internal server error",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "server error",
        "overloaded",
    ];
    hints.iter().any(|hint| lower.contains(hint))
}

pub fn is_model_not_found(err: &Error) -> bool {
    let lower = err.to_string().to_lowercase();
    lower.contains("model")
        && (lower.contains("not found")
            || lower.contains("does not exist")
            || lower.contains("unknown")
            || lower.contains("unsupported")
            || lower.contains("invalid"))
}

pub fn is_context_window_exceeded(err: &Error) -> bool {
    let lower = err.to_string().to_lowercase();
    let hints = [
        "exceeds the context window",
        "exceeds the available context size",
        "context window of this model",
        "maximum context length",
        "context length exceeded",
        "too many tokens",
        "token limit exceeded",
        "prompt is too long",
        "input is too long",
        "prompt exceeds max length",
    ];

    hints.iter().any(|hint| lower.contains(hint))
}

fn has_quota_business_hint(err: &Error) -> bool {
    let lower = err.to_string().to_lowercase();
    let hints = [
        "insufficient balance",
        "insufficient_balance",
        "insufficient quota",
        "insufficient_quota",
        "quota exhausted",
        "out of credits",
        "exceeded your current quota",
        "no available package",
        "package not active",
        "purchase package",
        "model not available for your plan",
        "doesn't include",
        "plan does not include",
    ];
    hints.iter().any(|hint| lower.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_auth_failed_401() {
        let err = anyhow::Error::msg(
            "Anthropic API error (401 Unauthorized): authentication_error: invalid x-api-key",
        );
        assert_eq!(classify_provider_error(&err), ProviderErrorKind::AuthFailed);
    }

    #[test]
    fn classify_rate_limited_429() {
        let err = anyhow::Error::msg("429 Too Many Requests: rate limit reached");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::RateLimited
        );
    }

    #[test]
    fn classify_quota_exceeded_business_error() {
        let err = anyhow::Error::msg("429: insufficient_quota, please check billing");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::QuotaExceeded
        );
    }

    #[test]
    fn classify_model_not_found() {
        let err = anyhow::Error::msg("The model 'gpt-5' does not exist");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::ModelNotFound
        );
    }

    #[test]
    fn classify_vision_not_supported() {
        let cap_err = zeroclaw_api::model_provider::ProviderCapabilityError {
            model_provider: "deepseek".into(),
            capability: "vision".into(),
            message: "provider 'deepseek' does not support vision".into(),
        };
        let err = anyhow::Error::from(cap_err);
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::VisionNotSupported
        );
    }

    #[test]
    fn classify_network_error_send_request() {
        let err = anyhow::Error::msg("reqwest error: error sending request for url");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::NetworkError
        );
    }

    #[test]
    fn classify_timeout_error() {
        let err = anyhow::Error::msg("LLM inference step timed out after 30s");
        assert_eq!(classify_provider_error(&err), ProviderErrorKind::Timeout);
    }

    #[test]
    fn classify_context_window_exceeded() {
        let err = anyhow::Error::msg(
            "OpenAI Codex stream error: Your input exceeds the context window of this model.",
        );
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::ContextWindowExceeded
        );
    }

    #[test]
    fn timeout_is_not_classified_as_network_error() {
        let err = anyhow::Error::msg("request timed out while waiting for upstream response");
        assert!(is_timeout_error(&err));
        assert!(!is_network_error(&err));
    }

    #[test]
    fn classify_server_error_500() {
        let err = anyhow::Error::msg("Anthropic API error (500 Internal Server Error)");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::ServerError
        );
    }

    #[test]
    fn classify_gateway_timeout_as_server_error() {
        let err = anyhow::Error::msg("504 Gateway Timeout");
        assert_eq!(
            classify_provider_error(&err),
            ProviderErrorKind::ServerError
        );
    }

    #[test]
    fn classify_provider_error_message_uses_string_path() {
        assert_eq!(
            classify_provider_error_message("429 Too Many Requests: rate limit reached"),
            ProviderErrorKind::RateLimited
        );
    }
}
