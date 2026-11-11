//! Raw iroh echo probe (Milestone 1). The only crate that names iroh directly.
//!
//! Adapted from iroh 1.4.1 `examples/echo.rs`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use iroh::endpoint::{Connection, RelayMode, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, RelayMap, RelayUrl};

const ALPN: &[u8] = b"triad/probe-echo/0";
const ADDR_PREFIX: &str = "probe1-";

/// Result of one echo round trip.
#[derive(Debug)]
pub struct ProbeReport {
    pub remote: String,
    pub path: String,
    pub connect_time: Duration,
    pub rtt: Duration,
    pub path_rtt: Option<Duration>,
    pub echoed: bool,
}

async fn bind(relay: &str) -> Result<Endpoint> {
    let url: RelayUrl = relay.parse().context("invalid relay URL")?;
    let endpoint = Endpoint::builder(presets::N0)
        .relay_mode(RelayMode::Custom(RelayMap::from(url)))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    Ok(endpoint)
}

/// Encodes an endpoint address as a copy-pasteable token.
pub fn encode_addr(addr: &EndpointAddr) -> Result<String> {
    Ok(format!(
        "{ADDR_PREFIX}{}",
        hex::encode(serde_json::to_vec(addr)?)
    ))
}

fn decode_addr(token: &str) -> Result<EndpointAddr> {
    let hex_part = token
        .trim()
        .strip_prefix(ADDR_PREFIX)
        .ok_or_else(|| anyhow!("address must start with {ADDR_PREFIX}"))?;
    let bytes = hex::decode(hex_part).context("address is not hex")?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Starts an echo endpoint, prints its address and serves until Ctrl-C.
pub async fn listen(relay: &str) -> Result<()> {
    let endpoint = bind(relay).await?;
    let router = Router::builder(endpoint).accept(ALPN, Echo).spawn();
    router.endpoint().online().await;
    println!("node    {}", router.endpoint().id());
    println!("addr    {}", encode_addr(&router.endpoint().addr())?);
    println!("Waiting for probe connections. Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;
    router.shutdown().await.map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Connects to a listening probe, echoes one line and reports the path.
pub async fn connect(relay: &str, addr: &str, line: &str) -> Result<ProbeReport> {
    let addr = decode_addr(addr)?;
    let endpoint = bind(relay).await?;
    endpoint.online().await;

    let started = Instant::now();
    let conn = endpoint.connect(addr, ALPN).await?;
    let connect_time = started.elapsed();

    // Give hole punching a moment so the report reflects the settled path.
    let mut echoed = true;
    let mut rtt = Duration::ZERO;
    for _ in 0..5 {
        let t = Instant::now();
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(line.as_bytes()).await?;
        send.finish()?;
        let response = recv.read_to_end(64 * 1024).await?;
        rtt = t.elapsed();
        echoed &= response == line.as_bytes();
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    let (path, path_rtt) = selected_path(&conn);
    let report = ProbeReport {
        remote: conn.remote_id().to_string(),
        path,
        connect_time,
        rtt,
        path_rtt,
        echoed,
    };

    conn.close(0u32.into(), b"bye");
    endpoint.close().await;
    Ok(report)
}

fn selected_path(conn: &Connection) -> (String, Option<Duration>) {
    let paths = conn.paths();
    for path in paths.iter() {
        if path.is_selected() {
            let kind = if path.is_relay() {
                "relayed"
            } else if path.is_ip() {
                "direct"
            } else {
                "other"
            };
            return (
                format!("{kind} ({:?})", path.remote_addr()),
                Some(path.rtt()),
            );
        }
    }
    ("unknown".to_string(), None)
}

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = connection.remote_id();
        println!("accepted connection from {remote}");
        loop {
            let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                break;
            };
            let bytes = tokio::io::copy(&mut recv, &mut send).await?;
            send.finish()?;
            println!("echoed {bytes} byte(s) to {}", remote.fmt_short());
        }
        Ok(())
    }
}
