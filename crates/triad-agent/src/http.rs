//! The one HTTP client the bots use, for the model provider and for HTTP MCP
//! servers alike.
//!
//! reqwest is built with `rustls-no-provider`, so TLS uses the ring provider
//! iroh already links instead of pulling in aws-lc. The system's root
//! certificates are used when there are any. A host with none (the VM guest
//! has no certificate store) falls back to the Mozilla roots bundled by
//! `webpki-root-certs`.

use std::sync::OnceLock;

/// Where the client's trusted roots came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roots {
    System,
    Bundled,
}

impl Roots {
    pub fn as_str(&self) -> &'static str {
        match self {
            Roots::System => "system",
            Roots::Bundled => "bundled",
        }
    }
}

/// Installs ring as the process's rustls provider. reqwest built without a
/// provider panics if none is installed, and rmcp builds clients of its own,
/// so this runs before any bot code touches HTTP. Idempotent.
pub fn install_crypto_provider() {
    // Fails only if a provider is already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The shared client and where its roots came from. Built once.
pub fn client() -> Result<(reqwest::Client, Roots), String> {
    static CLIENT: OnceLock<Result<(reqwest::Client, Roots), String>> = OnceLock::new();
    CLIENT.get_or_init(build).clone()
}

fn build() -> Result<(reqwest::Client, Roots), String> {
    install_crypto_provider();
    match reqwest::Client::builder().build() {
        Ok(client) => Ok((client, Roots::System)),
        Err(system_err) => {
            tracing::warn!("no system CA certificates ({system_err}); using bundled roots");
            bundled().map(|c| (c, Roots::Bundled))
        }
    }
}

/// A client that trusts only the bundled roots.
pub fn bundled() -> Result<reqwest::Client, String> {
    install_crypto_provider();
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    reqwest::Client::builder()
        .tls_certs_only(roots)
        .build()
        .map_err(|e| format!("building HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_system_or_bundled_roots() {
        let (_, roots) = client().unwrap();
        assert!(matches!(roots, Roots::System | Roots::Bundled));
        bundled().unwrap();
    }
}
