//! `triad inspect --grep`: scan the raw bytes p2panda stores for plaintext.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
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
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
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
