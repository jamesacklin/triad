//! `triad inspect --grep`: scan the raw bytes p2panda stores for plaintext.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use triad_core::net::P2PANDA_FILE;

/// Scans `p2panda.sqlite` and its WAL/SHM side files. Exit 0 if absent, 1 if found.
pub fn run(data_dir: &Path, needle: &str) -> Result<ExitCode> {
    if needle.is_empty() {
        bail!("--grep needs non-empty text");
    }
    let base = data_dir.join(P2PANDA_FILE);
    if !base.exists() {
        bail!("{} does not exist", base.display());
    }
    let mut found = Vec::new();
    let mut scanned = 0usize;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = data_dir.join(format!("{P2PANDA_FILE}{suffix}"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if !suffix.is_empty() && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        scanned += bytes.len();
        let n = count(&bytes, needle.as_bytes());
        if n > 0 {
            found.push(format!("{} ({n}x)", path.display()));
        }
    }
    if found.is_empty() {
        println!("absent: {needle:?} not found in {scanned} bytes");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("FOUND: {needle:?} in {}", found.join(", "));
        Ok(ExitCode::from(1))
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_sidecars_and_rejects_read_errors() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(P2PANDA_FILE);
        assert!(run(dir.path(), "secret").is_err());
        std::fs::write(&base, b"ciphertext").unwrap();
        assert_eq!(run(dir.path(), "secret").unwrap(), ExitCode::SUCCESS);
        let wal = dir.path().join(format!("{P2PANDA_FILE}-wal"));
        std::fs::write(&wal, b"secret").unwrap();
        assert_eq!(run(dir.path(), "secret").unwrap(), ExitCode::from(1));
        std::fs::remove_file(&wal).unwrap();
        std::fs::create_dir(&wal).unwrap();
        assert!(run(dir.path(), "secret").is_err());
        assert!(run(dir.path(), "").is_err());
    }
}
