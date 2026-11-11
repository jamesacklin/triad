//! The per-install identity: p2panda credentials in `credentials.cbor`.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::envelope::{cbor_decode, cbor_encode};

pub const CREDENTIALS_FILE: &str = "credentials.cbor";

/// A 32-byte Ed25519 public key, used as node ID and MLS identity.
pub type NodeId = [u8; 32];

/// The local identity. Wraps p2panda credentials without exposing their type.
#[derive(Clone)]
pub struct Identity {
    pub(crate) credentials: p2panda::Credentials,
}

impl Identity {
    /// Loads `credentials.cbor` from the data directory, creating it if missing.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data directory {}", data_dir.display()))?;
        let path = data_dir.join(CREDENTIALS_FILE);
        if path.exists() {
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let credentials: p2panda::Credentials =
                cbor_decode(&bytes).context("credentials.cbor is corrupt")?;
            return Ok(Identity { credentials });
        }
        let credentials = p2panda::Credentials::generate();
        let bytes = cbor_encode(&credentials)?;
        write_private(&path, &bytes)?;
        tracing::info!("created new identity");
        Ok(Identity { credentials })
    }

    pub fn node_id(&self) -> NodeId {
        *self.credentials.verifying_key().as_bytes()
    }

    pub fn node_hex(&self) -> String {
        hex::encode(self.node_id())
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}

/// Full lowercase hex of a node ID.
pub fn hex_id(id: &NodeId) -> String {
    hex::encode(id)
}

/// First 8 hex characters, as shown in chat.
pub fn short_id(id: &NodeId) -> String {
    hex::encode(&id[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path()).unwrap();
        let b = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(a.node_id(), b.node_id());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(CREDENTIALS_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
