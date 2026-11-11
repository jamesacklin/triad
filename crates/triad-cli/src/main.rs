//! The `triad` binary (Milestone 1: probe only).

use clap::{Parser, Subcommand};

const DEFAULT_RELAY: &str = "https://use1-1.relay.n0.iroh.link./";

#[derive(Parser)]
#[command(name = "triad")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Milestone 1 raw iroh probe.
    Probe {
        #[command(subcommand)]
        cmd: ProbeCmd,
    },
}

#[derive(Subcommand)]
enum ProbeCmd {
    /// Start a raw iroh echo endpoint and print its address.
    Listen {
        #[arg(long)]
        relay: Option<String>,
    },
    /// Connect, echo one line, report whether the path was direct or relayed.
    Connect {
        addr: String,
        #[arg(long)]
        relay: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Probe { cmd } => match cmd {
            ProbeCmd::Listen { relay } => {
                triad_probe::listen(relay.as_deref().unwrap_or(DEFAULT_RELAY)).await?
            }
            ProbeCmd::Connect { addr, relay } => {
                let relay = relay.as_deref().unwrap_or(DEFAULT_RELAY);
                let r = triad_probe::connect(relay, &addr, "triad probe echo").await?;
                println!("remote  {}", r.remote);
                println!("echo    {}", if r.echoed { "ok" } else { "MISMATCH" });
                println!("path    {}", r.path);
                println!("connect {:?}", r.connect_time);
                println!("rtt     {:?}", r.rtt);
            }
        },
    }
    Ok(())
}
