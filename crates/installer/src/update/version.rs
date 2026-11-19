//! Parsing release tags and comparing them with the installed version.

use std::cmp::Ordering;

use anyhow::Context;
use semver::Version;

/// Strips the conventional `v` prefix from a release tag (`v0.2.0` -> `0.2.0`).
pub fn strip_tag_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Turns user input (`0.2.0` or `v0.2.0`) into a release tag (`v0.2.0`).
pub fn to_tag(version: &str) -> String {
    format!("v{}", strip_tag_prefix(version.trim()))
}

/// Parses a version or release tag as semver.
pub fn parse(version_or_tag: &str) -> anyhow::Result<Version> {
    let raw = strip_tag_prefix(version_or_tag.trim());
    Version::parse(raw).with_context(|| format!("`{version_or_tag}` is not a valid version"))
}

/// How a release relates to the installed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// The release is newer than what is installed.
    Newer,
    /// The release is the installed version.
    Same,
    /// The release is older than what is installed.
    Older,
}

/// Compares a release version with the installed one using semver
/// precedence, so `0.2.0-rc.1` is older than `0.2.0`.
pub fn compare(installed: &Version, release: &Version) -> Comparison {
    match release.cmp_precedence(installed) {
        Ordering::Greater => Comparison::Newer,
        Ordering::Equal => Comparison::Same,
        Ordering::Less => Comparison::Older,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(installed: &str, release: &str) -> Comparison {
        compare(&parse(installed).unwrap(), &parse(release).unwrap())
    }

    #[test]
    fn tag_prefix_is_optional() {
        assert_eq!(parse("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(to_tag("1.2.3"), "v1.2.3");
        assert_eq!(to_tag("v1.2.3"), "v1.2.3");
        assert!(parse("latest").is_err());
    }

    #[test]
    fn compares_release_versions() {
        assert_eq!(cmp("0.1.0", "v0.2.0"), Comparison::Newer);
        assert_eq!(cmp("0.2.0", "v0.2.0"), Comparison::Same);
        assert_eq!(cmp("0.10.0", "v0.9.9"), Comparison::Older);
    }

    #[test]
    fn pre_releases_precede_the_release() {
        assert_eq!(cmp("0.2.0-rc.1", "v0.2.0"), Comparison::Newer);
        assert_eq!(cmp("0.2.0", "v0.2.0-rc.1"), Comparison::Older);
        assert_eq!(cmp("0.2.0-rc.1", "v0.2.0-rc.2"), Comparison::Newer);
        assert_eq!(cmp("0.1.0", "v0.2.0-beta"), Comparison::Newer);
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert_eq!(cmp("0.2.0+abc", "v0.2.0"), Comparison::Same);
    }
}
