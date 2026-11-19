//! Extracting release archives (`.tar.gz` on Unix, `.zip` on Windows) with
//! protection against path traversal.

use std::path::{Component, Path, PathBuf};

use anyhow::bail;

/// Validates an archive entry path and returns it relative to the
/// extraction root, or `None` for the root itself (`./`).
///
/// Absolute paths, drive prefixes and `..` components are rejected so an
/// archive can never write outside the extraction directory.
pub fn safe_relative_path(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "archive entry `{}` points outside the archive",
                    path.display()
                )
            }
        }
    }
    Ok((!relative.as_os_str().is_empty()).then_some(relative))
}

/// Extracts the release archive at `archive` into `dest` using the format
/// of this platform.
pub fn extract(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive)?;
    #[cfg(unix)]
    return extract_tar_gz(file, dest);
    #[cfg(windows)]
    return extract_zip(file, dest);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, dest);
        bail!("extracting release archives is not supported on this platform")
    }
}

/// Unpacks a gzip-compressed tar stream into `dest`. Only regular files and
/// directories are accepted; links and special files are rejected.
#[cfg(unix)]
pub fn extract_tar_gz(reader: impl std::io::Read, dest: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    use tar::EntryType;

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    for entry in archive.entries().context("reading archive")? {
        let mut entry = entry.context("reading archive entry")?;
        let path = entry.path()?.into_owned();
        let Some(relative) = safe_relative_path(&path)? else {
            continue;
        };
        let target = dest.join(&relative);
        match entry.header().entry_type() {
            EntryType::Directory => std::fs::create_dir_all(&target)?,
            EntryType::Regular | EntryType::Continuous => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                entry
                    .unpack(&target)
                    .with_context(|| format!("extracting {}", relative.display()))?;
            }
            other => bail!(
                "archive entry `{}` has unsupported type {other:?}",
                path.display()
            ),
        }
    }
    Ok(())
}

/// Unpacks a zip archive into `dest`. Symlinks are rejected.
#[cfg(windows)]
pub fn extract_zip(reader: impl std::io::Read + std::io::Seek, dest: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    let mut archive = zip::ZipArchive::new(reader).context("reading archive")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("reading archive entry")?;
        let name = entry.name().to_owned();
        let Some(relative) = safe_relative_path(Path::new(&name))? else {
            continue;
        };
        let target = dest.join(&relative);
        if entry.is_symlink() {
            bail!("archive entry `{name}` is a symlink");
        } else if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&target)?;
            std::io::copy(&mut entry, &mut out).with_context(|| format!("extracting {name}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(
            safe_relative_path(Path::new("./plugins/a.wasm")).unwrap(),
            Some(PathBuf::from("plugins").join("a.wasm"))
        );
        assert_eq!(safe_relative_path(Path::new("./")).unwrap(), None);
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_relative_path(Path::new("../evil")).is_err());
        assert!(safe_relative_path(Path::new("plugins/../../evil")).is_err());
        assert!(safe_relative_path(Path::new("/etc/passwd")).is_err());
        #[cfg(windows)]
        assert!(safe_relative_path(Path::new(r"C:\evil")).is_err());
    }

    /// Builds a `.tar.gz` from `(path, contents, mode)` entries, writing the
    /// raw name so even unsafe paths can be produced.
    #[cfg(unix)]
    fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for (path, data, mode) in entries {
            let mut header = tar::Header::new_gnu();
            let name = &mut header.as_old_mut().name;
            name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn extracts_tar_gz_with_modes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let data = tar_gz(&[
            ("oxid-relay", b"bin", 0o755),
            ("./plugins/a.wasm", b"wasm", 0o644),
        ]);
        extract_tar_gz(data.as_slice(), dir.path()).unwrap();
        let binary = dir.path().join("oxid-relay");
        assert_eq!(std::fs::read(&binary).unwrap(), b"bin");
        assert_eq!(
            std::fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::read(dir.path().join("plugins/a.wasm")).unwrap(),
            b"wasm"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_traversal_is_rejected() {
        for evil in ["../evil", "plugins/../../evil", "/tmp/evil"] {
            let root = tempfile::tempdir().unwrap();
            let dest = root.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let data = tar_gz(&[(evil, b"x", 0o644)]);
            let err = extract_tar_gz(data.as_slice(), &dest).unwrap_err();
            assert!(err.to_string().contains("outside"), "{evil}: {err}");
            assert!(!root.path().join("evil").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_symlinks_are_rejected() {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "link", "/etc/passwd")
            .unwrap();
        let data = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = extract_tar_gz(data.as_slice(), dir.path()).unwrap_err();
        assert!(err.to_string().contains("unsupported type"), "{err}");
    }

    /// Builds a zip from `(name, contents)` entries.
    #[cfg(windows)]
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[cfg(windows)]
    #[test]
    fn extracts_zip() {
        let dir = tempfile::tempdir().unwrap();
        let data = zip_bytes(&[("oxid-relay.exe", b"bin"), ("plugins/a.wasm", b"wasm")]);
        extract_zip(std::io::Cursor::new(data), dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("oxid-relay.exe")).unwrap(),
            b"bin"
        );
        assert_eq!(
            std::fs::read(dir.path().join("plugins").join("a.wasm")).unwrap(),
            b"wasm"
        );
    }

    #[cfg(windows)]
    #[test]
    fn zip_traversal_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let data = zip_bytes(&[("../evil", b"x")]);
        assert!(extract_zip(std::io::Cursor::new(data), &dest).is_err());
        assert!(!root.path().join("evil").exists());
    }
}
