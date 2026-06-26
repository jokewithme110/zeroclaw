//! Integration tests for the SkillHub source / URL helpers.
//!
//! These were originally an in-file `mod skillhub_helpers_tests` in
//! `crates/zeroclaw-runtime/src/skills/mod.rs`. They were promoted to an
//! integration test so they:
//!   * run as a standalone binary (`cargo test -p zeroclaw-runtime --test skillhub`),
//!     not blocked by the 26 pre-existing lib-test compile errors;
//!   * live next to the other integration test
//!     `tests/scheduled_no_conversation_leak_5415.rs`;
//!   * keep `skills/mod.rs` focused on production code.
//!
//! All tested items are `pub` in `skills/mod.rs` (and re-exported), so the
//! `use super::*;` -> `use zeroclaw_runtime::skills::{...};` change is the
//! only API-surface delta.

use zeroclaw_runtime::skills::{
    is_skillhub_source, parse_http_skill_url, parse_skillhub_source, skill_dir_name,
    skillhub_download_url, skillhub_skill_dir_name, validate_skill_slug,
};

// ── is_skillhub_source ──────────────────────────────────────────────

#[test]
fn is_skillhub_source_accepts_clawhub_prefix() {
    assert!(is_skillhub_source("clawhub:foo"));
    assert!(is_skillhub_source("clawhub:foo@1.0.0"));
    assert!(is_skillhub_source("clawhub:foo-bar_baz"));
}

#[test]
fn is_skillhub_source_accepts_any_https_url() {
    // PR1 拆了 host 白名单：任意 host 都接受
    assert!(is_skillhub_source(
        "https://skillhubictst.mec189.cn/enhance/foo"
    ));
    assert!(is_skillhub_source("https://example.com/owner/skill"));
    assert!(is_skillhub_source("http://internal.lan/skill"));
}

#[test]
fn is_skillhub_source_rejects_other_schemes_and_garbage() {
    assert!(!is_skillhub_source("file:///etc/passwd"));
    assert!(!is_skillhub_source("ftp://example.com/skill"));
    assert!(!is_skillhub_source("git@github.com:foo/bar.git"));
    assert!(!is_skillhub_source(""));
    assert!(!is_skillhub_source("just-a-name"));
}

#[test]
fn legacy_clawhub_short_prefix_still_works() {
    // 改名为 is_skillhub_source 但 clawhub: 前缀语义保持
    assert!(is_skillhub_source("clawhub:foo"));
    assert!(is_skillhub_source("clawhub:foo@1.0.0"));
}

#[test]
fn legacy_clawhub_dot_ai_url_still_accepted() {
    // PR1 删了 CLAWHUB_DOMAIN / CLAWHUB_WWW_DOMAIN 白名单，
    // 但 clawhub.ai 的 URL 形式仍然走通（碰到合法 URL 都返回 true）
    assert!(is_skillhub_source("https://clawhub.ai/owner/foo"));
    assert!(is_skillhub_source("https://www.clawhub.ai/foo"));
    assert!(is_skillhub_source("http://clawhub.ai/foo"));
}

// ── parse_skillhub_source ───────────────────────────────────────────

#[test]
fn parse_skillhub_source_short_form_no_version() {
    assert_eq!(
        parse_skillhub_source("clawhub:foo").unwrap(),
        ("foo".to_string(), None)
    );
}

#[test]
fn parse_skillhub_source_short_form_with_semver() {
    assert_eq!(
        parse_skillhub_source("clawhub:foo@1.0.0").unwrap(),
        ("foo".to_string(), Some("1.0.0".to_string()))
    );
}

#[test]
fn parse_skillhub_source_short_form_with_ict_version() {
    // 实测 ICT 平台用 yyyyMMdd.HHmmss
    assert_eq!(
        parse_skillhub_source("clawhub:foo@20260528.071446").unwrap(),
        ("foo".to_string(), Some("20260528.071446".to_string()))
    );
}

#[test]
fn parse_skillhub_source_url_form_takes_last_path_segment() {
    // URL 形式：取 path 末段当 slug，@ 不切（避免误把 query 段当 version）
    assert_eq!(
        parse_skillhub_source("https://example.com/owner/foo").unwrap(),
        ("foo".to_string(), None)
    );
    assert_eq!(
        parse_skillhub_source("https://example.com/owner/sub/foo").unwrap(),
        ("foo".to_string(), None)
    );
}

#[test]
fn parse_skillhub_source_rejects_invalid_inputs() {
    assert!(parse_skillhub_source("clawhub:foo@").is_err());
    assert!(parse_skillhub_source("clawhub:@1.0.0").is_err());
    assert!(parse_skillhub_source("clawhub:").is_err());
    assert!(parse_skillhub_source("clawhub:/foo").is_err());
}

// ── skillhub_download_url ───────────────────────────────────────────

#[test]
fn skillhub_download_url_uses_provided_base() {
    let url = skillhub_download_url(
        "https://skillhubictst.mec189.cn/enhance",
        "attendance-query-lite",
        "20260528.071446",
    );
    assert_eq!(
        url,
        "https://skillhubictst.mec189.cn/enhance/api/v1/download?slug=attendance-query-lite&version=20260528.071446"
    );
}

#[test]
fn skillhub_download_url_strips_trailing_slash() {
    let url = skillhub_download_url("https://clawhub.ai/", "foo", "1.0.0");
    assert_eq!(
        url,
        "https://clawhub.ai/api/v1/download?slug=foo&version=1.0.0"
    );
}

#[test]
fn skillhub_download_url_encodes_slug_and_version() {
    // Regression test for URL injection: an agent passing
    // slug="foo&evil=bar" must not be able to inject a second
    // query parameter. Same for version with reserved chars.
    let url = skillhub_download_url("https://hub.example", "foo&evil=bar", "1.0.0&x=1");
    assert_eq!(
        url,
        "https://hub.example/api/v1/download?slug=foo%26evil%3Dbar&version=1.0.0%26x%3D1"
    );
    // Also cover `?` and `#` (which would otherwise truncate the path).
    let url2 = skillhub_download_url("https://hub.example", "foo?bar", "v#1");
    assert!(!url2.contains("?bar"), "raw `?` must be encoded: {url2}");
    assert!(!url2.contains("#1"), "raw `#` must be encoded: {url2}");
}

// ── skillhub_skill_dir_name ─────────────────────────────────────────

#[test]
fn skillhub_skill_dir_name_normalizes_hyphens() {
    // kebab-case -> snake_case (与旧行为一致)
    let name = skillhub_skill_dir_name("clawhub:attendance-query-lite").unwrap();
    assert_eq!(name, "attendance_query_lite");
}

#[test]
fn skillhub_skill_dir_name_handles_url_form() {
    let name = skillhub_skill_dir_name("https://example.com/owner/attendance-query-lite").unwrap();
    assert_eq!(name, "attendance_query_lite");
}

// ── parse_http_skill_url ────────────────────────────────────────────

#[test]
fn parse_http_skill_url_accepts_https_and_http() {
    assert!(parse_http_skill_url("https://example.com/x").is_some());
    assert!(parse_http_skill_url("http://example.com/x").is_some());
}

#[test]
fn parse_http_skill_url_rejects_other_schemes() {
    assert!(parse_http_skill_url("file:///etc/passwd").is_none());
    assert!(parse_http_skill_url("ftp://example.com/x").is_none());
    assert!(parse_http_skill_url("not-a-url").is_none());
}

#[test]
fn parse_http_skill_url_accepts_any_host() {
    // 关键测试：拆了 host 白名单后必须仍然走通
    assert!(parse_http_skill_url("https://clawhub.ai/owner/skill").is_some());
    assert!(parse_http_skill_url("https://www.clawhub.ai/skill").is_some());
    assert!(parse_http_skill_url("https://skillhubictst.mec189.cn/enhance/foo").is_some());
    assert!(parse_http_skill_url("https://internal.lan/skill").is_some());
}

// ── validate_skill_slug (R1) ─────────────────────────────────────────

#[test]
fn validate_skill_slug_accepts_canonical_form() {
    assert!(validate_skill_slug("foo-bar_baz.v2").is_ok());
    assert!(validate_skill_slug("attendance-query-lite").is_ok());
    assert!(validate_skill_slug("a").is_ok());
}

#[test]
fn validate_skill_slug_rejects_empty_and_traversal() {
    assert!(validate_skill_slug("").is_err());
    assert!(validate_skill_slug("../etc/passwd").is_err());
    assert!(validate_skill_slug("foo/../bar").is_err());
    assert!(validate_skill_slug("foo\\bar").is_err());
    assert!(validate_skill_slug("C:\\Windows").is_err());
}

#[test]
fn validate_skill_slug_rejects_non_ascii_and_specials() {
    assert!(validate_skill_slug("foo bar").is_err());
    assert!(validate_skill_slug("foo$bar").is_err());
    assert!(validate_skill_slug("foo;rm").is_err());
}

#[test]
fn parse_skillhub_source_now_runs_slug_through_validator() {
    // `$` is now rejected at the parse layer, not silently later.
    assert!(parse_skillhub_source("clawhub:foo$bar").is_err());
    assert!(parse_skillhub_source("clawhub:foo bar").is_err());
    assert!(parse_skillhub_source("clawhub:..").is_err());
    // URL form: last path segment is validated the same way.
    assert!(parse_skillhub_source("https://hub.example.com/..").is_err());
    assert!(parse_skillhub_source("https://hub.example.com/foo$bar").is_err());
}

// ── skill_dir_name (R3) ──────────────────────────────────────────────

#[test]
fn skill_dir_name_lowercases_and_normalizes() {
    assert_eq!(
        skill_dir_name("Attendance-Query-Lite"),
        "attendance_query_lite"
    );
    assert_eq!(skill_dir_name("foo"), "foo");
    assert_eq!(skill_dir_name("foo-bar"), "foo_bar");
    assert_eq!(skill_dir_name("foo.v2"), "foo.v2");
}

#[test]
fn skill_dir_name_falls_back_to_lit_skill_for_empty() {
    // empty -> "skill"; but `---` survives as `___` (filter keeps `_`),
    // and `..` survives as `..` (filter keeps `.`). Callers are expected
    // to have already run `validate_skill_slug` (which rejects `..`).
    assert_eq!(skill_dir_name(""), "skill");
    assert_eq!(skill_dir_name("---"), "___");
    assert_eq!(skill_dir_name(".."), "..");
    // truly empty-after-filter: only non-[A-Za-z0-9_.] chars (e.g. spaces)
    assert_eq!(skill_dir_name("   "), "skill");
    assert_eq!(skill_dir_name("foo bar"), "foobar");
}
