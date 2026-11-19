//! `oxid-relay update`: fetch, verify and apply the latest GitHub release.
//!
//! The flow is split into small steps so each can be tested on its own:
//! [`release`] looks the release up and picks the asset, [`checksum`]
//! verifies the download against `SHA256SUMS`, [`archive`] unpacks it and
//! [`apply`] swaps the binary with rollback. All network access goes through
//! [`http::HttpClient`].

mod apply;
mod archive;
mod checksum;
mod http;
mod release;
mod version;

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::bundle::{Bundle, CopyOptions};
use crate::layout::InstallLayout;
use http::{HttpClient, ReqwestClient};
use release::{CHECKSUMS_ASSET, Release};
use version::Comparison;

/// Options for [`update`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Only report installed and available versions.
    pub check: bool,
    /// Reinstall even if the installed version is current or newer.
    pub force: bool,
    /// Install this release tag (e.g. `v0.2.0`) instead of the latest.
    pub version: Option<String>,
}

/// Runs the update according to `options`.
pub fn update(options: &UpdateOptions) -> anyhow::Result<()> {
    let http = ReqwestClient::new()?;
    let Some(plan) = plan(&http, options, crate::VERSION, crate::TARGET)? else {
        return Ok(());
    };

    let layout = InstallLayout::current()?;
    apply::ensure_installed(&layout)?;
    ensure_writable(&layout.bin_dir)?;
    let service = crate::service::current()?;

    let workdir = tempfile::tempdir().context("creating a temporary directory")?;
    let archive = download_verified(&http, &plan, workdir.path())?;
    println!("Extracting {} ...", plan.asset);
    let extracted = workdir.path().join("bundle");
    archive::extract(&archive, &extracted).with_context(|| format!("extracting {}", plan.asset))?;
    let bundle = Bundle::from_dir(&extracted)?;

    let applied = apply::apply(
        &layout,
        service.as_ref(),
        &bundle.binary(),
        || {
            println!("Updating plugins and example config ...");
            bundle.copy_into(
                &layout,
                CopyOptions {
                    binary: false,
                    seed_config: false,
                },
            )
        },
        &apply::StartupCheck::default(),
    )?;

    println!(
        "Updated OxidRelay {} -> {}.",
        crate::VERSION,
        plan.release.version()
    );
    if let Some(backup) = applied.backup {
        println!(
            "The previous binary was kept as {} and is removed by the next update.",
            backup.display()
        );
    }
    Ok(())
}

/// A release selected for installation.
#[derive(Debug)]
struct Plan {
    /// The release to install.
    release: Release,
    /// Name of the archive asset for this platform.
    asset: String,
}

/// Looks up the requested release and decides whether to install it.
///
/// Prints the outcome and returns `None` when there is nothing to do
/// (`--check`, already up to date, or newer than the requested release).
fn plan(
    http: &dyn HttpClient,
    options: &UpdateOptions,
    installed: &str,
    target: &str,
) -> anyhow::Result<Option<Plan>> {
    let tag = options.version.as_deref().map(version::to_tag);
    println!("Checking GitHub for releases of {} ...", crate::GITHUB_REPO);
    let release = release::fetch(http, crate::GITHUB_REPO, tag.as_deref())?;
    if let Some(tag) = &tag {
        release::ensure_tag(&release, tag)?;
    }

    let installed_version = version::parse(installed)?;
    let comparison = version::compare(&installed_version, &version::parse(&release.tag_name)?);
    let label = if tag.is_some() { "Requested" } else { "Latest" };

    if options.check {
        println!("Installed: {installed}");
        println!("{label}:    {}", release.version());
        println!(
            "{}",
            match comparison {
                Comparison::Newer => "An update is available; run `oxid-relay update`.",
                Comparison::Same => "Already up to date.",
                Comparison::Older => "The installed version is newer.",
            }
        );
        return Ok(None);
    }

    if !options.force {
        match comparison {
            Comparison::Newer => {}
            Comparison::Same => {
                println!("OxidRelay {installed} is already up to date.");
                return Ok(None);
            }
            Comparison::Older => {
                println!(
                    "Installed version {installed} is newer than {}; use --force to downgrade.",
                    release.version()
                );
                return Ok(None);
            }
        }
    }

    let asset = release::asset_name(release.version(), target);
    // Fail before downloading anything if this platform is not published.
    release.asset(&asset)?;
    release.asset(CHECKSUMS_ASSET)?;
    Ok(Some(Plan { release, asset }))
}

/// Downloads the planned archive and `SHA256SUMS` into `dir`, verifies the
/// checksum and returns the path of the archive. Nothing is written if the
/// checksum does not match.
fn download_verified(http: &dyn HttpClient, plan: &Plan, dir: &Path) -> anyhow::Result<PathBuf> {
    let download = |name: &str| -> anyhow::Result<Vec<u8>> {
        println!("Downloading {name} ...");
        let asset = plan.release.asset(name)?;
        http.get(&asset.browser_download_url)
            .with_context(|| format!("downloading {name}"))
    };
    let data = download(&plan.asset)?;
    let sums = download(CHECKSUMS_ASSET)?;
    let sums = String::from_utf8(sums).context("SHA256SUMS is not valid UTF-8")?;

    println!("Verifying checksum ...");
    checksum::verify(&data, &sums, &plan.asset)?;

    let path = dir.join(&plan.asset);
    std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Fails early, before downloading, when the binary directory is not
/// writable (typically: not running as root / Administrator).
fn ensure_writable(dir: &Path) -> anyhow::Result<()> {
    tempfile::tempfile_in(dir)
        .map(drop)
        .map_err(|e| apply::io_error(e, format!("writing to {}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::FakeHttp;

    const TARGET: &str = "x86_64-unknown-linux-gnu";
    const ASSET: &str = "oxid-relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz";

    /// Release JSON for `tag` with the Linux archive and `SHA256SUMS`.
    fn release_json(tag: &str) -> String {
        let version = version::strip_tag_prefix(tag);
        serde_json::json!({
            "tag_name": tag,
            "assets": [
                {"name": format!("oxid-relay-v{version}-{TARGET}.tar.gz"),
                 "browser_download_url": "https://dl.test/archive"},
                {"name": "SHA256SUMS", "browser_download_url": "https://dl.test/sums"},
            ]
        })
        .to_string()
    }

    fn latest(tag: &str) -> FakeHttp {
        FakeHttp::default().with(
            &release::release_url(crate::GITHUB_REPO, None),
            release_json(tag),
        )
    }

    fn opts(check: bool, force: bool, version: Option<&str>) -> UpdateOptions {
        UpdateOptions {
            check,
            force,
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn plans_newer_release() {
        let plan = plan(
            &latest("v0.2.0"),
            &opts(false, false, None),
            "0.1.0",
            TARGET,
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.asset, ASSET);
    }

    #[test]
    fn check_and_up_to_date_do_nothing() {
        let http = latest("v0.2.0");
        assert!(
            plan(&http, &opts(true, false, None), "0.1.0", TARGET)
                .unwrap()
                .is_none()
        );
        assert!(
            plan(&http, &opts(false, false, None), "0.2.0", TARGET)
                .unwrap()
                .is_none()
        );
        assert!(
            plan(&http, &opts(false, false, None), "0.3.0", TARGET)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn force_reinstalls_current_version() {
        let plan = plan(&latest("v0.2.0"), &opts(false, true, None), "0.2.0", TARGET).unwrap();
        assert!(plan.is_some());
    }

    #[test]
    fn requested_version_uses_tag_endpoint() {
        let http = FakeHttp::default().with(
            &release::release_url(crate::GITHUB_REPO, Some("v0.2.0")),
            release_json("v0.2.0"),
        );
        let plan = plan(&http, &opts(false, false, Some("0.2.0")), "0.1.0", TARGET).unwrap();
        assert_eq!(plan.unwrap().release.tag_name, "v0.2.0");
    }

    #[test]
    fn unpublished_target_fails_before_download() {
        let err = plan(
            &latest("v0.2.0"),
            &opts(false, false, None),
            "0.1.0",
            "riscv64gc-unknown-linux-gnu",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("riscv64gc"), "{err}");
        assert!(err.contains(ASSET), "{err}");
    }

    fn planned(http: &FakeHttp) -> Plan {
        plan(http, &opts(false, false, None), "0.1.0", TARGET)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn downloads_and_verifies_archive() {
        let sums = format!("{}  {ASSET}\n", checksum::sha256_hex(b"archive"));
        let http = latest("v0.2.0")
            .with("https://dl.test/archive", "archive")
            .with("https://dl.test/sums", sums);
        let dir = tempfile::tempdir().unwrap();
        let path = download_verified(&http, &planned(&http), dir.path()).unwrap();
        assert_eq!(path, dir.path().join(ASSET));
        assert_eq!(std::fs::read(path).unwrap(), b"archive");
    }

    #[test]
    fn checksum_mismatch_writes_nothing() {
        let sums = format!("{}  {ASSET}\n", checksum::sha256_hex(b"other"));
        let http = latest("v0.2.0")
            .with("https://dl.test/archive", "archive")
            .with("https://dl.test/sums", sums);
        let dir = tempfile::tempdir().unwrap();
        let err = download_verified(&http, &planned(&http), dir.path()).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn failed_download_is_reported() {
        let http = latest("v0.2.0").with_status("https://dl.test/archive", 500);
        let dir = tempfile::tempdir().unwrap();
        let err = download_verified(&http, &planned(&http), dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("HTTP 500"), "{err:#}");
    }

    #[test]
    fn unwritable_dir_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_writable(dir.path()).is_ok());
        assert!(ensure_writable(&dir.path().join("missing")).is_err());
    }
}
