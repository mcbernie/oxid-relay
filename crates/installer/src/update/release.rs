//! Looking up GitHub releases and picking the asset for this platform.

use anyhow::{Context, bail};
use serde::Deserialize;

use super::http::{GITHUB_API, HttpClient, HttpError, get_json};
use super::version::strip_tag_prefix;

/// Name of the checksum asset published with every release.
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// A GitHub release, reduced to the fields the updater needs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    /// Release tag, e.g. `v0.2.0`.
    pub tag_name: String,
    /// Files attached to the release.
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    /// File name, e.g. `oxid-relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz`.
    pub name: String,
    /// Public download URL.
    pub browser_download_url: String,
}

impl Release {
    /// The version part of the tag (`v0.2.0` -> `0.2.0`).
    pub fn version(&self) -> &str {
        strip_tag_prefix(&self.tag_name)
    }

    /// Finds an asset by exact name, listing the available ones otherwise.
    pub fn asset(&self, name: &str) -> anyhow::Result<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .with_context(|| {
                let available: Vec<&str> = self.assets.iter().map(|a| a.name.as_str()).collect();
                let available = if available.is_empty() {
                    "none".to_owned()
                } else {
                    available.join(", ")
                };
                format!(
                    "release {} has no asset {name} (available: {available})",
                    self.tag_name
                )
            })
    }
}

/// API URL of the latest release, or of the release tagged `tag`.
pub fn release_url(repo: &str, tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("{GITHUB_API}/repos/{repo}/releases/tags/{tag}"),
        None => format!("{GITHUB_API}/repos/{repo}/releases/latest"),
    }
}

/// Name of the release archive for `version` (without `v`) and `target`.
pub fn asset_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("oxid-relay-v{version}-{target}.{ext}")
}

/// Fetches the latest release of `repo`, or the one tagged `tag`.
pub fn fetch(http: &dyn HttpClient, repo: &str, tag: Option<&str>) -> anyhow::Result<Release> {
    let url = release_url(repo, tag);
    get_json(http, &url).map_err(|err| match err.downcast_ref::<HttpError>() {
        Some(HttpError::Status { code: 404, .. }) => match tag {
            Some(tag) => anyhow::anyhow!("release {tag} not found in {repo}"),
            None => anyhow::anyhow!("{repo} has no published release yet"),
        },
        Some(HttpError::Status {
            code: 403 | 429,
            rate_limited,
            ..
        }) => {
            let reason = if *rate_limited {
                "the GitHub API rate limit is exhausted"
            } else {
                "GitHub refused the request (possibly rate limited)"
            };
            anyhow::anyhow!("{reason}; try again later ({err})")
        }
        _ => err.context(format!("could not look up the release of {repo}")),
    })
}

/// Fails unless `release` matches the requested tag, guarding against
/// surprising API answers.
pub fn ensure_tag(release: &Release, tag: &str) -> anyhow::Result<()> {
    if release.tag_name != tag {
        bail!(
            "asked GitHub for release {tag} but got {}",
            release.tag_name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::http::FakeHttp;

    const JSON: &str = r#"{
        "tag_name": "v0.2.0",
        "name": "OxidRelay 0.2.0",
        "draft": false,
        "assets": [
            {"name": "SHA256SUMS", "size": 10,
             "browser_download_url": "https://example.test/SHA256SUMS"},
            {"name": "oxid-relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz", "size": 99,
             "browser_download_url": "https://example.test/linux.tar.gz"}
        ]
    }"#;

    #[test]
    fn builds_api_urls() {
        assert_eq!(
            release_url("o/r", None),
            "https://api.github.com/repos/o/r/releases/latest"
        );
        assert_eq!(
            release_url("o/r", Some("v1.0.0")),
            "https://api.github.com/repos/o/r/releases/tags/v1.0.0"
        );
    }

    #[test]
    fn asset_name_per_target() {
        assert_eq!(
            asset_name("0.2.0", "x86_64-unknown-linux-gnu"),
            "oxid-relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name("0.2.0", "aarch64-unknown-linux-musl"),
            "oxid-relay-v0.2.0-aarch64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_name("0.2.0-rc.1", "x86_64-pc-windows-msvc"),
            "oxid-relay-v0.2.0-rc.1-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn parses_release_json() {
        let release: Release = serde_json::from_str(JSON).unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.version(), "0.2.0");
        assert_eq!(release.assets.len(), 2);
        let asset = release
            .asset("oxid-relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz")
            .unwrap();
        assert_eq!(
            asset.browser_download_url,
            "https://example.test/linux.tar.gz"
        );
    }

    #[test]
    fn missing_asset_lists_available_ones() {
        let release: Release = serde_json::from_str(JSON).unwrap();
        let err = release.asset("nope.zip").unwrap_err().to_string();
        assert!(err.contains("nope.zip"), "{err}");
        assert!(err.contains("SHA256SUMS, oxid-relay-v0.2.0"), "{err}");
    }

    #[test]
    fn fetches_latest_and_tagged_release() {
        let http = FakeHttp::default()
            .with(&release_url("o/r", None), JSON)
            .with(&release_url("o/r", Some("v0.2.0")), JSON);
        assert_eq!(fetch(&http, "o/r", None).unwrap().tag_name, "v0.2.0");
        let tagged = fetch(&http, "o/r", Some("v0.2.0")).unwrap();
        ensure_tag(&tagged, "v0.2.0").unwrap();
        assert!(ensure_tag(&tagged, "v0.3.0").is_err());
    }

    #[test]
    fn explains_not_found_and_rate_limit() {
        let http = FakeHttp::default().with_status(&release_url("o/r", None), 403);
        let err = fetch(&http, "o/r", Some("v9.9.9")).unwrap_err().to_string();
        assert!(err.contains("release v9.9.9 not found"), "{err}");
        let err = fetch(&http, "o/r", None).unwrap_err().to_string();
        assert!(err.contains("rate limited"), "{err}");
        let http = FakeHttp::default().with_status(&release_url("o/r", None), 429);
        let err = fetch(&http, "o/r", None).unwrap_err().to_string();
        assert!(err.contains("rate limit is exhausted"), "{err}");
    }

    #[test]
    fn rejects_invalid_json() {
        let http = FakeHttp::default().with(&release_url("o/r", None), "<html>");
        assert!(fetch(&http, "o/r", None).is_err());
    }
}
