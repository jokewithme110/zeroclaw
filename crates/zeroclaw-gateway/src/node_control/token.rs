//! Token generation and paired-token recovery for node control.

use zeroclaw_config::secrets::SecretStore;

/// Prefix for generated auth tokens.
const TOKEN_PREFIX: &str = "zc_";

/// Result of recovering the gateway paired-token state for auto-discovery.
#[derive(Debug, Clone)]
pub struct PairedTokenInitialization {
    pub paired_tokens: Vec<String>,
    pub primary_token: Option<String>,
    pub needs_persist: bool,
}

/// Generate a cryptographically secure random token.
pub fn generate_auth_token() -> String {
    format!("{}{}", TOKEN_PREFIX, uuid::Uuid::new_v4().simple())
}

/// Resolve runtime `gateway.paired_tokens` for auto-discovery.
///
/// The result keeps only recoverable plaintext tokens. Legacy hash-only values
/// are dropped from the runtime discovery set because they cannot be decrypted
/// or published back to nodes. When the stored data is missing, contains
/// decrypt failures, uses legacy `enc:` entries, or contains unrecoverable hash
/// values, a fresh token is appended and callers should persist the updated
/// vector back to config.
pub fn resolve_paired_tokens(
    secret_store: &SecretStore,
    stored_tokens: &[String],
) -> PairedTokenInitialization {
    let mut paired_tokens = Vec::new();
    let mut needs_persist = false;

    for raw_token in stored_tokens {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }

        if looks_like_legacy_token_hash(token) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "event": "node_control_legacy_hashed_paired_token_detected",
                    })),
                "Ignoring legacy hashed paired_token for auto-discovery and generating a recoverable replacement"
            );
            needs_persist = true;
            continue;
        }

        if SecretStore::needs_migration(token) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Migrate)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "event": "node_control_paired_token_migration_detected",
                    })),
                "Legacy enc: paired_token detected; a fresh discovery token will be appended"
            );
            needs_persist = true;
        }

        let plaintext = if SecretStore::is_encrypted(token) {
            match secret_store.decrypt(token) {
                Ok(plaintext) => plaintext,
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "event": "node_control_paired_token_decrypt_failed",
                                "error": error.to_string(),
                            })),
                        "Ignoring unreadable paired_token and generating a recoverable replacement"
                    );
                    needs_persist = true;
                    continue;
                }
            }
        } else {
            token.to_string()
        };

        push_unique_token(&mut paired_tokens, plaintext);
    }

    if paired_tokens.is_empty() || needs_persist {
        let token = generate_auth_token();
        push_unique_token(&mut paired_tokens, token);
        needs_persist = true;
    }

    let primary_token = paired_tokens.first().cloned();

    PairedTokenInitialization {
        paired_tokens,
        primary_token,
        needs_persist,
    }
}

fn push_unique_token(tokens: &mut Vec<String>, token: String) {
    if !tokens.iter().any(|existing| existing == &token) {
        tokens.push(token);
    }
}

fn looks_like_legacy_token_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_token_format() {
        let token = generate_auth_token();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + 32);
    }

    #[test]
    fn test_generate_token_uniqueness() {
        let token1 = generate_auth_token();
        let token2 = generate_auth_token();
        assert_ne!(token1, token2);
    }

    #[test]
    fn resolve_generates_new_token_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let result = resolve_paired_tokens(&store, &[]);
        assert!(result.needs_persist);
        assert_eq!(result.paired_tokens.len(), 1);
        assert!(result.primary_token.unwrap().starts_with(TOKEN_PREFIX));
    }

    #[test]
    fn resolve_keeps_existing_plaintext_token() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let result = resolve_paired_tokens(&store, &["zc_existing".into()]);
        assert!(!result.needs_persist);
        assert_eq!(result.paired_tokens, vec!["zc_existing"]);
        assert_eq!(result.primary_token.as_deref(), Some("zc_existing"));
    }

    #[test]
    fn resolve_decrypts_existing_encrypted_token() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("zc_existing").unwrap();

        let result = resolve_paired_tokens(&store, &[encrypted]);
        assert!(!result.needs_persist);
        assert_eq!(result.paired_tokens, vec!["zc_existing"]);
        assert_eq!(result.primary_token.as_deref(), Some("zc_existing"));
    }

    #[test]
    fn resolve_appends_new_token_when_legacy_hash_is_detected() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let legacy_hash = hex::encode([0xabu8; 32]);

        let result = resolve_paired_tokens(&store, &[legacy_hash]);
        assert!(result.needs_persist);
        assert_eq!(result.paired_tokens.len(), 1);
        assert!(result.primary_token.unwrap().starts_with(TOKEN_PREFIX));
    }

    #[test]
    fn resolve_appends_new_token_when_decrypt_fails() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let result = resolve_paired_tokens(&store, &["enc2:not-valid-hex".into()]);
        assert!(result.needs_persist);
        assert_eq!(result.paired_tokens.len(), 1);
        assert!(result.primary_token.unwrap().starts_with(TOKEN_PREFIX));
    }

    #[test]
    fn resolve_preserves_existing_token_and_appends_new_one_for_legacy_migration() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let legacy = make_legacy_encrypted_value(&store, tmp.path(), "zc_existing");

        let result = resolve_paired_tokens(&store, &[legacy]);
        assert!(result.needs_persist);
        assert_eq!(result.paired_tokens.len(), 2);
        assert_eq!(result.paired_tokens[0], "zc_existing");
        assert!(result.paired_tokens[1].starts_with(TOKEN_PREFIX));
        assert_eq!(result.primary_token.as_deref(), Some("zc_existing"));
    }

    fn make_legacy_encrypted_value(
        store: &SecretStore,
        root: &std::path::Path,
        plaintext: &str,
    ) -> String {
        let _ = store.encrypt("setup").unwrap();
        let key_hex = std::fs::read_to_string(root.join(".secret_key")).unwrap();
        let key = hex::decode(key_hex.trim()).unwrap();
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        format!("enc:{}", hex::encode(ciphertext))
    }

    fn xor_cipher(input: &[u8], key: &[u8]) -> Vec<u8> {
        input
            .iter()
            .zip(key.iter().cycle())
            .map(|(byte, key_byte)| byte ^ key_byte)
            .collect()
    }
}
