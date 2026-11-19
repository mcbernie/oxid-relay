//! Parsing `SHA256SUMS` files and verifying downloads against them.

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 digest of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parses `sha256sum` output: lines of `<hex>  <name>` (text mode) or
/// `<hex> *<name>` (binary mode). Blank lines are skipped; malformed lines
/// are an error so a corrupted file is never half-trusted.
pub fn parse_sums(text: &str) -> anyhow::Result<Vec<(String, String)>> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            parse_line(line).with_context(|| format!("SHA256SUMS line {}: `{line}`", index + 1))
        })
        .collect()
}

/// Parses one `SHA256SUMS` line into `(hex digest, file name)`.
fn parse_line(line: &str) -> anyhow::Result<(String, String)> {
    let line = line.trim_end_matches('\r');
    let (hex, rest) = line
        .split_once(' ')
        .context("expected `<sha256>  <file name>`")?;
    let name = rest
        .strip_prefix(' ')
        .or_else(|| rest.strip_prefix('*'))
        .context("expected two spaces or ` *` between digest and file name")?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("`{hex}` is not a SHA-256 hex digest");
    }
    if name.is_empty() {
        bail!("missing file name");
    }
    Ok((hex.to_ascii_lowercase(), name.to_owned()))
}

/// Checks that `data` matches the digest listed for `name` in `sums`.
pub fn verify(data: &[u8], sums: &str, name: &str) -> anyhow::Result<()> {
    let entries = parse_sums(sums)?;
    let expected = entries
        .iter()
        .find(|(_, entry)| entry == name)
        .map(|(hex, _)| hex)
        .with_context(|| format!("SHA256SUMS has no entry for {name}"))?;
    let actual = sha256_hex(data);
    if &actual != expected {
        bail!("checksum mismatch for {name}: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the empty input.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_known_value() {
        assert_eq!(sha256_hex(b""), EMPTY);
    }

    #[test]
    fn parses_text_and_binary_mode_lines() {
        let sums = format!("{EMPTY}  a.tar.gz\n\n{} *b.zip\r\n", EMPTY.to_uppercase());
        let entries = parse_sums(&sums).unwrap();
        assert_eq!(
            entries,
            vec![
                (EMPTY.to_owned(), "a.tar.gz".to_owned()),
                (EMPTY.to_owned(), "b.zip".to_owned()),
            ]
        );
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(parse_sums("nothex  a.tar.gz").is_err());
        assert!(parse_sums(&format!("{EMPTY} a.tar.gz")).is_err());
        assert!(parse_sums(&format!("{EMPTY}  ")).is_err());
        assert!(parse_sums(EMPTY).is_err());
    }

    #[test]
    fn verifies_matching_digest() {
        let sums = format!("{}  other\n{EMPTY}  a.tar.gz\n", sha256_hex(b"x"));
        verify(b"", &sums, "a.tar.gz").unwrap();
    }

    #[test]
    fn rejects_mismatch_and_missing_entry() {
        let sums = format!("{EMPTY}  a.tar.gz\n");
        let err = verify(b"tampered", &sums, "a.tar.gz").unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        let err = verify(b"", &sums, "b.tar.gz").unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
    }
}
