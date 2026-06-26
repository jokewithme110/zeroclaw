//! HTTP client + serde models for the SkillHub REST API.
//!
//! Reference: `skillhub-api.md` (ICT self-hosted production, 2026-06-24).
//! All fields are `#[serde(default)]` + `alias` to tolerate schema drift
//! between upstream `clawhub.ai` and the ICT self-hosted instance.

use serde::Deserialize;

/// `GET /api/v1/skills` — list response (with `nextCursor` pagination).
#[derive(Debug, Clone, Deserialize)]
pub struct SkillListResponse {
    #[serde(default)]
    pub items: Vec<SkillListItem>,
    #[serde(default, alias = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillListItem {
    pub slug: String,
    #[serde(default, alias = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
    #[serde(default)]
    pub stats: Option<SkillStats>,
    #[serde(default, alias = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(default, alias = "updatedAt")]
    pub updated_at: Option<i64>,
    #[serde(default, alias = "latestVersion")]
    pub latest_version: Option<SkillVersion>,
}

/// `GET /api/v1/search?q=...` — search response. NOTE: uses `results`, not
/// `items` — the list and search endpoints have different shapes.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillSearchResponse {
    #[serde(default)]
    pub results: Vec<SkillSearchItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillSearchItem {
    pub slug: String,
    #[serde(default, alias = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Top-level version string from search results. May be `null` for
    /// skills that have never been published.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default, alias = "updatedAt")]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillStats {
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub stars: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillVersion {
    /// Version string. May be `null` for unpublished or broken records.
    /// Format is NOT fixed: ICT self-hosted skills use `yyyyMMdd.HHmmss`,
    /// imported skills use semver. Treated as opaque string end-to-end.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, alias = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = r#"{
        "items": [
            {
                "slug": "attendance-query-lite",
                "displayName": "attendance-query-lite",
                "summary": "\u8003\u52e4\u67e5\u8be2\u5de5\u5177",
                "tags": {},
                "stats": { "downloads": 21, "stars": 0 },
                "createdAt": 0,
                "updatedAt": 1779952486606,
                "latestVersion": {
                    "version": "20260528.071446",
                    "createdAt": 1779952486606,
                    "changelog": "",
                    "license": null
                }
            }
        ],
        "nextCursor": null
    }"#;

    const SEARCH_FIXTURE: &str = r#"{
        "results": [
            {
                "slug": "attendance-query-lite",
                "displayName": "attendance-query-lite",
                "summary": "\u8003\u52e4\u67e5\u8be2",
                "version": "20260528.071446",
                "score": 0.85,
                "updatedAt": 1779952486606
            }
        ]
    }"#;

    #[test]
    fn list_response_parses_ict_fixture() {
        let resp: SkillListResponse = serde_json::from_str(LIST_FIXTURE).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].slug, "attendance-query-lite");
        assert_eq!(
            resp.items[0]
                .latest_version
                .as_ref()
                .unwrap()
                .version
                .as_deref()
                .unwrap(),
            "20260528.071446"
        );
        assert_eq!(resp.items[0].stats.as_ref().unwrap().downloads, 21);
        assert!(resp.next_cursor.is_none());
    }

    #[test]
    fn search_response_uses_results_key_not_items() {
        let resp: SkillSearchResponse = serde_json::from_str(SEARCH_FIXTURE).unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].slug, "attendance-query-lite");
        assert_eq!(
            resp.results[0].version.as_deref().unwrap(),
            "20260528.071446"
        );
    }

    #[test]
    fn list_response_handles_missing_optional_fields() {
        let minimal = r#"{"items": [{"slug": "foo"}]}"#;
        let resp: SkillListResponse = serde_json::from_str(minimal).unwrap();
        assert_eq!(resp.items[0].slug, "foo");
        assert!(resp.items[0].latest_version.is_none());
        assert!(resp.items[0].stats.is_none());
    }

    #[test]
    fn search_response_handles_missing_score_and_updated_at() {
        let minimal = r#"{"results": [{"slug": "foo", "version": "1.0.0"}]}"#;
        let resp: SkillSearchResponse = serde_json::from_str(minimal).unwrap();
        assert_eq!(resp.results[0].slug, "foo");
        assert!(resp.results[0].score.is_none());
    }
}
