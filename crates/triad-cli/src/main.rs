//! The `triad` binary.

mod chat;
mod inspect;
mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use triad_core::envelope::Invite;
use triad_core::identity::{Identity, hex_id};
use triad_core::net::DEFAULT_RELAY;
use triad_core::state::{JoinStatus, Role, State};

#[derive(Parser)]
#[command(
    name = "triad",
    version,
    about = "Two-node chat on iroh, MLS and p2panda"
)]
struct Cli {
    /// Use this data directory.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Also print sync and connection events.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the node ID. Create credentials if the data directory is new.
    Whoami,
    /// Create the group, print the invite, enter chat mode.
    Create {
        /// Override the default relay.
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Accept every valid join request without asking.
        #[arg(long)]
        auto_accept: bool,
        /// Print one JSON object per event instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the invite again. Creator only.
    Invite,
    /// Save the invite, publish a KeyPackage, enter chat mode.
    Join {
        invite: String,
        /// Print one JSON object per event instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Resume the existing group in chat mode.
    Chat {
        /// Accept every valid join request without asking.
        #[arg(long)]
        auto_accept: bool,
        /// Print one JSON object per event instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print stored decrypted messages, oldest first.
    History {
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Print role, join status, MLS epoch, member IDs and operation counts.
    Status,
    /// Scan the raw bytes of p2panda.sqlite for the text. Exit 0 if absent, 1 if found.
    Inspect {
        #[arg(long, value_name = "TEXT")]
        grep: String,
    },
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
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Connect, echo one line, report whether the path was direct or relayed.
    Connect {
        addr: String,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
}

fn default_data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; pass --data-dir")?;
    let home = PathBuf::from(home);
    if cfg!(target_os = "macos") {
        Ok(home.join("Library/Application Support/triad"))
    } else {
        Ok(home.join(".local/share/triad"))
    }
}

fn init_logging(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("triad.log"))?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .init();
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => default_data_dir()?,
    };
    if !matches!(cli.cmd, Cmd::Probe { .. } | Cmd::Inspect { .. }) {
        init_logging(&data_dir)?;
    }
    let rt = tokio::runtime::Runtime::new()?;
    let result = run_command(cli.cmd, &data_dir, cli.verbose, &rt);
    // Bound shutdown on errors and early returns as well as normal exits.
    rt.shutdown_timeout(std::time::Duration::from_millis(500));
    result
}

fn run_command(
    cmd: Cmd,
    data_dir: &Path,
    verbose: bool,
    rt: &tokio::runtime::Runtime,
) -> Result<ExitCode> {
    match cmd {
        Cmd::Whoami => {
            let id = Identity::load_or_create(data_dir)?;
            println!("{}", id.node_hex());
        }
        Cmd::Create {
            relay,
            auto_accept,
            json,
        } => {
            let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
            let opts = chat::Options {
                auto_accept,
                json,
                verbose,
            };
            rt.block_on(chat::create(data_dir, &relay, opts))?;
        }
        Cmd::Invite => {
            let state = State::open(data_dir)?;
            let meta = state.meta()?.context("no group in this data directory")?;
            if meta.role != Role::Creator {
                bail!("only the creator can print the invite");
            }
            println!(
                "{}",
                Invite::new(meta.topic, meta.creator, &meta.relay).render()?
            );
        }
        Cmd::Join { invite, json } => {
            let invite = Invite::parse(&invite)?;
            let opts = chat::Options {
                auto_accept: false,
                json,
                verbose,
            };
            rt.block_on(chat::join(data_dir, invite, opts))?;
        }
        Cmd::Chat { auto_accept, json } => {
            let opts = chat::Options {
                auto_accept,
                json,
                verbose,
            };
            rt.block_on(chat::resume(data_dir, opts))?;
        }
        Cmd::History { limit } => {
            let state = State::open(data_dir)?;
            let me = Identity::load_or_create(data_dir)?.node_id();
            for m in state.history(limit)? {
                let who = if m.author == me {
                    "me".to_string()
                } else {
                    triad_core::identity::short_id(&m.author)
                };
                let ts = m.op_ts.map(|t| t / 1000).unwrap_or(m.sent_at_ms);
                println!("[{}] {}: {}", output::clock(ts), who, m.text);
            }
        }
        Cmd::Status => {
            let state = State::open(data_dir)?;
            let me = Identity::load_or_create(data_dir)?;
            let Some(meta) = state.meta()? else {
                println!("node    {}", me.node_hex());
                println!("status  none (no group in this data directory)");
                return Ok(ExitCode::SUCCESS);
            };
            let counts = state.counts()?;
            println!("node    {}", me.node_hex());
            println!("role    {}", meta.role.as_str());
            println!("status  {}", meta.status.as_str());
            println!("topic   {}", hex::encode(meta.topic));
            println!("creator {}", hex_id(&meta.creator));
            match chat::mls_summary(data_dir, &me, &meta) {
                Ok(Some((epoch, members))) => {
                    println!("epoch   {epoch}");
                    println!("members {}", members.len());
                    for m in members {
                        println!("        {}", hex_id(&m));
                    }
                }
                Ok(None) => println!("epoch   (no MLS group)"),
                Err(e) => println!("epoch   (error: {e:#})"),
            }
            println!(
                "ops     ok={} skipped={} failed={}",
                counts.ok, counts.skipped, counts.failed
            );
            if meta.status == JoinStatus::Pending {
                println!("hint    waiting for the creator's Welcome");
            }
        }
        Cmd::Inspect { grep } => {
            return inspect::run(data_dir, &grep);
        }
        Cmd::Probe { cmd } => match cmd {
            ProbeCmd::Listen { relay } => {
                let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
                rt.block_on(triad_probe::listen(&relay))?;
            }
            ProbeCmd::Connect { addr, relay } => {
                let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
                let r = rt.block_on(triad_probe::connect(&relay, &addr, "triad probe echo"))?;
                println!("remote  {}", r.remote);
                println!("echo    {}", if r.echoed { "ok" } else { "MISMATCH" });
                println!("path    {}", r.path);
                println!("connect {:?}", r.connect_time);
                println!("rtt     {:?} (last echo)", r.rtt);
                if let Some(p) = r.path_rtt {
                    println!("pathrtt {p:?} (QUIC estimate)");
                }
                if !r.echoed {
                    return Ok(ExitCode::from(1));
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}
