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

/// Only assets under this prefix are ever downloaded.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/abu-abbas/gpui-like-toad/releases/download/";

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// The asset for this OS: `.dmg` on macOS, `.exe` on Windows, the plain
/// `linux-x86_64` binary on Linux. `os` is `std::env::consts::OS`.
pub fn pick_asset<'a>(assets: &'a [Asset], os: &str) -> Option<&'a Asset> {
    let wanted = |name: &str| match os {
        "macos" => name.ends_with(".dmg"),
        "windows" => name.ends_with("windows-x86_64.exe"),
        "linux" => name.ends_with("linux-x86_64"),
        _ => false,
    };
    assets.iter().find(|a| wanted(&a.name))
}

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub url: String,
    pub assets: Vec<Asset>,
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
    // Keep only assets with a safe file name hosted on this repo's
    // release downloads.
    let assets = json
        .get("assets")
        .and_then(Json::as_arr)
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.to_string();
            let url = a.get("browser_download_url")?.as_str()?.to_string();
            let safe = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
            (safe && url.starts_with(DOWNLOAD_PREFIX)).then(|| Asset {
                name,
                url,
                size: a.get("size").and_then(Json::as_u64).unwrap_or(0),
            })
        })
        .collect();
    Some(ReleaseInfo {
        assets,
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
    fn picks_asset_per_os_and_rejects_foreign_hosts() {
        let base = DOWNLOAD_PREFIX;
        let body = format!(
            r#"{{"tag_name":"v0.2.0","html_url":"https://github.com/abu-abbas/gpui-like-toad/releases/tag/v0.2.0",
            "assets":[
             {{"name":"Ora-The-Explorer-0.2.0-macos-arm64.dmg","browser_download_url":"{base}v0.2.0/a.dmg","size":5}},
             {{"name":"Ora-The-Explorer-0.2.0-windows-x86_64.exe","browser_download_url":"{base}v0.2.0/w.exe","size":6}},
             {{"name":"Ora-The-Explorer-0.2.0-linux-x86_64","browser_download_url":"{base}v0.2.0/l","size":7}},
             {{"name":"evil","browser_download_url":"http://evil.test/x","size":1}},
             {{"name":"../x","browser_download_url":"{base}v0.2.0/x","size":1}}]}}"#
        );
        let r = parse_release(&body).unwrap();
        assert_eq!(r.assets.len(), 3);
        assert!(pick_asset(&r.assets, "macos")
            .unwrap()
            .name
            .ends_with(".dmg"));
        assert!(pick_asset(&r.assets, "windows")
            .unwrap()
            .name
            .ends_with(".exe"));
        assert_eq!(pick_asset(&r.assets, "linux").unwrap().size, 7);
        assert!(pick_asset(&r.assets, "freebsd").is_none());
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
