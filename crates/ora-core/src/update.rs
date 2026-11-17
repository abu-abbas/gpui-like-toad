//! Update check against GitHub Releases: pure logic only (version
//! parsing/comparison, release JSON parsing, rate limiting). The HTTP
//! request itself lives in the app crate. The check is a plain anonymous
//! GET of the public "latest release" endpoint; no user data is sent.

use crate::store::{parse_json, Json};

/// Public endpoint of the latest (non-draft, non-prerelease) release.
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/abu-abbas/gpui-like-toad/releases/latest";

/// Minimum time between automatic checks at startup.
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// `MAJOR.MINOR.PATCH`, compared numerically (so 0.1.10 > 0.1.9, unlike a
/// string compare). Releases are tagged `vX.Y.Z` by the Release workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Accepts an optional leading `v`; ignores any `-pre`/`+build` suffix.
    pub fn parse(text: &str) -> Option<Version> {
        let t = text.trim().trim_start_matches(['v', 'V']);
        let t = t.split(['-', '+']).next()?;
        let mut parts = t.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version(major, minor, patch))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub url: String,
}

/// Parse the GitHub "latest release" JSON. `None` if it lacks a usable tag
/// or page URL. Drafts and prereleases are skipped.
pub fn parse_release(body: &str) -> Option<ReleaseInfo> {
    let json = parse_json(body).ok()?;
    let flag = |k: &str| json.get(k).and_then(Json::as_bool).unwrap_or(false);
    if flag("draft") || flag("prerelease") {
        return None;
    }
    let tag = json.get("tag_name")?.as_str()?.to_string();
    let url = json.get("html_url")?.as_str()?.to_string();
    if !url.starts_with("https://github.com/") {
        return None;
    }
    let text = |k: &str| {
        json.get(k)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(ReleaseInfo {
        version: Version::parse(&tag)?,
        name: text("name"),
        notes: text("body"),
        tag,
        url,
    })
}

/// The release if it is strictly newer than `current`.
pub fn newer_than(release: ReleaseInfo, current: &str) -> Option<ReleaseInfo> {
    let current = Version::parse(current)?;
    (release.version > current).then_some(release)
}

/// Whether the startup check should hit the network now.
pub fn should_auto_check(auto_check: bool, last_checked: u64, now: u64) -> bool {
    auto_check && now.saturating_sub(last_checked) >= AUTO_CHECK_INTERVAL_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"tag_name":"v0.1.10","name":"Ora The Explorer 0.1.10",
        "html_url":"https://github.com/abu-abbas/gpui-like-toad/releases/tag/v0.1.10",
        "draft":false,"prerelease":false,"body":"- fix a\n- fix b"}"#;

    #[test]
    fn versions_compare_numerically() {
        assert!(Version::parse("v0.1.10") > Version::parse("0.1.9"));
        assert_eq!(Version::parse("v1.2.3-rc1"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("nightly"), None);
    }

    #[test]
    fn parses_release_and_detects_newer() {
        let r = parse_release(BODY).unwrap();
        assert_eq!(r.version, Version(0, 1, 10));
        assert!(newer_than(r.clone(), "0.1.9").is_some());
        assert!(newer_than(r.clone(), "0.1.10").is_none());
        assert!(newer_than(r, "0.2.0").is_none());
    }

    #[test]
    fn rejects_drafts_prereleases_and_foreign_urls() {
        assert!(parse_release(&BODY.replace(r#""draft":false"#, r#""draft":true"#)).is_none());
        assert!(
            parse_release(&BODY.replace(r#""prerelease":false"#, r#""prerelease":true"#)).is_none()
        );
        assert!(parse_release(&BODY.replace("https://github.com/", "http://evil.test/")).is_none());
        assert!(parse_release("not json").is_none());
    }

    #[test]
    fn auto_check_is_rate_limited() {
        assert!(should_auto_check(true, 0, 1_000_000));
        assert!(!should_auto_check(true, 1_000_000, 1_000_100));
        assert!(should_auto_check(
            true,
            1_000_000,
            1_000_000 + AUTO_CHECK_INTERVAL_SECS
        ));
        assert!(!should_auto_check(false, 0, 1_000_000));
    }
}
