//! `triad-spaces`: the triad chat on p2panda's built-in encrypted spaces, with
//! no OpenMLS (Milestone 6). Same invite format, same JSON events, same
//! `state.sqlite` history; p2panda does group key agreement and encryption.
//!
//! Join flow: the joiner subscribes to the space, which announces its key
//! bundle on the space topic. The creator sees that as a join request and adds
//! the member with `Space::add`.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use p2panda::node::AckPolicy;
use p2panda::spaces::{Space, SpaceSubscription};
use p2panda::streams::StreamEvent;
use p2panda::{AccessLevel, Hash, Node, RelayUrl, VerifyingKey};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use triad_core::envelope::{ChatBody, Invite};
use triad_core::identity::{Identity, NodeId, hex_id, short_id};
use triad_core::net::{DEFAULT_RELAY, P2PANDA_FILE};
use triad_core::state::{JoinStatus, Meta, Outcome, Role, State, StoredMessage, now_ms};

#[derive(Parser)]
#[command(
    name = "triad-spaces",
    version,
    about = "triad chat on p2panda encrypted spaces"
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
    /// Print the node ID.
    Whoami,
    /// Create the space, print the invite, enter chat mode.
    Create {
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        #[arg(long)]
        auto_accept: bool,
        #[arg(long)]
        json: bool,
    },
    /// Subscribe to the space from an invite and wait to be added.
    Join {
        invite: String,
        #[arg(long)]
        json: bool,
    },
    /// Resume the existing space in chat mode.
    Chat {
        #[arg(long)]
        auto_accept: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print stored messages, oldest first.
    History {
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Print role, join status and operation counts.
    Status,
    /// Scan p2panda.sqlite for the text. Exit 0 if absent, 1 if found.
    Inspect {
        #[arg(long, value_name = "TEXT")]
        grep: String,
    },
}

#[derive(Clone, Copy)]
struct Opts {
    auto_accept: bool,
    json: bool,
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run_cli(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn default_data_dir() -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    Ok(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/triad-spaces")
    } else {
        home.join(".local/share/triad-spaces")
    })
}

fn run_cli(cli: Cli) -> Result<ExitCode> {
    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => default_data_dir()?,
    };
    std::fs::create_dir_all(&data_dir)?;
    if !matches!(cli.cmd, Cmd::Inspect { .. }) {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join("triad.log"))?;
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    }
    let rt = tokio::runtime::Runtime::new()?;
    let verbose = cli.verbose;
    match cli.cmd {
        Cmd::Whoami => println!("{}", Identity::load_or_create(&data_dir)?.node_hex()),
        Cmd::Create {
            relay,
            auto_accept,
            json,
        } => {
            let relay = relay.unwrap_or_else(|| DEFAULT_RELAY.to_string());
            let opts = Opts {
                auto_accept,
                json,
                verbose,
            };
            rt.block_on(chat(&data_dir, Start::Create { relay }, opts))?;
        }
        Cmd::Join { invite, json } => {
            let invite = Invite::parse(&invite)?;
            let opts = Opts {
                auto_accept: false,
                json,
                verbose,
            };
            rt.block_on(chat(&data_dir, Start::Join(invite), opts))?;
        }
        Cmd::Chat { auto_accept, json } => {
            let opts = Opts {
                auto_accept,
                json,
                verbose,
            };
            rt.block_on(chat(&data_dir, Start::Resume, opts))?;
        }
        Cmd::History { limit } => {
            let state = State::open(&data_dir)?;
            let me = Identity::load_or_create(&data_dir)?.node_id();
            for m in state.history(limit)? {
                let who = if m.author == me {
                    "me".into()
                } else {
                    short_id(&m.author)
                };
                println!("{}: {}", who, m.text);
            }
        }
        Cmd::Status => {
            let state = State::open(&data_dir)?;
            let me = Identity::load_or_create(&data_dir)?;
            println!("node    {}", me.node_hex());
            if let Some(meta) = state.meta()? {
                let c = state.counts()?;
                println!("role    {}", meta.role.as_str());
                println!("status  {}", meta.status.as_str());
                println!("space   {}", hex::encode(meta.topic));
                println!(
                    "ops     ok={} skipped={} failed={}",
                    c.ok, c.skipped, c.failed
                );
            } else {
                println!("status  none");
            }
        }
        Cmd::Inspect { grep } => return inspect(&data_dir, &grep),
    }
    rt.shutdown_timeout(Duration::from_millis(500));
    Ok(ExitCode::SUCCESS)
}

fn inspect(data_dir: &Path, needle: &str) -> Result<ExitCode> {
    if needle.is_empty() {
        bail!("--grep needs non-empty text");
    }
    let mut found = 0;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        if let Ok(bytes) = std::fs::read(data_dir.join(format!("{P2PANDA_FILE}{suffix}"))) {
            found += bytes
                .windows(needle.len())
                .filter(|w| *w == needle.as_bytes())
                .count();
        }
    }
    if found == 0 {
        println!("absent: {needle:?}");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("FOUND: {needle:?} ({found}x)");
        Ok(ExitCode::from(1))
    }
}

enum Start {
    Create { relay: String },
    Join(Invite),
    Resume,
}

struct Printer {
    json: bool,
    verbose: bool,
}

impl Printer {
    fn emit(&self, value: serde_json::Value, text: impl FnOnce() -> String) {
        if self.json {
            println!("{value}");
        } else {
            println!("{}", text());
        }
    }

    fn info(&self, text: &str) {
        self.emit(json!({"event": "info", "text": text}), || text.to_string());
    }

    fn error(&self, text: &str) {
        self.emit(json!({"event": "error", "text": text}), || {
            format!("error: {text}")
        });
    }
}

fn key(id: &NodeId) -> Result<VerifyingKey> {
    VerifyingKey::from_bytes(id).context("invalid node ID")
}

async fn chat(data_dir: &Path, start: Start, opts: Opts) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let me = identity.node_id();
    let state = State::open(data_dir)?;
    let existing = state.meta()?;
    let (relay, bootstrap) = match (&start, &existing) {
        (Start::Create { .. } | Start::Join(_), Some(_)) => {
            bail!("this data directory already holds a space; use `chat` or a fresh --data-dir")
        }
        (Start::Resume, None) => bail!("no space in this data directory"),
        (Start::Create { relay }, None) => (relay.clone(), None),
        (Start::Join(inv), None) => (inv.relay.clone(), Some(inv.creator)),
        (Start::Resume, Some(m)) => (
            m.relay.clone(),
            (m.role == Role::Member).then_some(m.creator),
        ),
    };

    let relay_url: RelayUrl = relay.parse().context("invalid relay URL")?;
    let db = data_dir.join(P2PANDA_FILE);
    let mut builder = p2panda::builder()
        .credentials(identity.p2panda_credentials())
        .database_url(&format!("sqlite://{}?mode=rwc", db.display()))
        .ack_policy(AckPolicy::Explicit)
        .relay_url(relay_url.clone());
    if let Some(b) = bootstrap {
        builder = builder.bootstrap(key(&b)?, relay_url);
    }
    let node = builder.spawn().await.context("spawning p2panda node")?;

    let mut pending = None;
    let (space, rx, meta) = match start {
        Start::Create { relay } => {
            let (space, rx) = node.create_space::<ChatBody>().await?;
            let topic = *space.id().as_bytes();
            let meta = Meta {
                topic,
                group_id: topic.to_vec(),
                creator: me,
                relay,
                role: Role::Creator,
                status: JoinStatus::Joined,
                key_package_published: false,
                joined_epoch: None,
            };
            state.insert_meta(&meta)?;
            (Some(space), rx, meta)
        }
        Start::Join(inv) => {
            let meta = Meta {
                topic: inv.topic,
                group_id: inv.topic.to_vec(),
                creator: inv.creator,
                relay: inv.relay,
                role: Role::Member,
                status: JoinStatus::Pending,
                key_package_published: false,
                joined_epoch: None,
            };
            state.insert_meta(&meta)?;
            let (p, rx) = node.space::<ChatBody>(Hash::from(inv.topic)).await?;
            pending = Some(p);
            (None, rx, meta)
        }
        Start::Resume => {
            let meta = existing.expect("checked above");
            let (p, rx) = node.space::<ChatBody>(Hash::from(meta.topic)).await?;
            pending = Some(p);
            (None, rx, meta)
        }
    };

    let printer = Printer {
        json: opts.json,
        verbose: opts.verbose,
    };
    let invite = (meta.role == Role::Creator)
        .then(|| Invite::new(meta.topic, meta.creator, &meta.relay).render())
        .transpose()?;
    printer.emit(
        json!({
            "event": "ready", "node": hex_id(&me), "role": meta.role.as_str(),
            "status": meta.status.as_str(), "epoch": null, "members": null,
            "group": hex::encode(meta.topic), "invite": invite,
        }),
        || {
            let mut s = format!(
                "node    {}\nspace   {}",
                short_id(&me),
                hex::encode(&meta.topic[..4])
            );
            if let Some(i) = &invite {
                s.push_str(&format!("\ninvite  {i}"));
            }
            s.push_str("\nType a message, or /quit.");
            s
        },
    );

    let is_creator = meta.role == Role::Creator;
    let mut session = Session {
        me,
        node,
        state,
        meta,
        space,
        printer,
        opts,
        // A joiner announces itself (and the creator) once it is in.
        members: if is_creator {
            BTreeSet::from([me])
        } else {
            BTreeSet::new()
        },
        asks: VecDeque::new(),
    };
    let joined = async move {
        match pending {
            Some(p) => p.joined().await,
            None => std::future::pending().await,
        }
    };
    let result = session.event_loop(rx, joined).await;
    if let Some(space) = session.space.take() {
        let _ = space.close().await;
    }
    drop(session.node);
    result
}

struct Session {
    me: NodeId,
    node: Node,
    state: State,
    meta: Meta,
    space: Option<Space<ChatBody>>,
    printer: Printer,
    opts: Opts,
    members: BTreeSet<NodeId>,
    asks: VecDeque<NodeId>,
}

fn spawn_stdin() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .blocking_send(line.trim_end_matches(['\n', '\r']).to_string())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    rx
}

impl Session {
    async fn event_loop<F, E>(
        &mut self,
        mut rx: SpaceSubscription<ChatBody>,
        joined: F,
    ) -> Result<()>
    where
        F: std::future::Future<Output = Result<Space<ChatBody>, E>>,
        E: std::fmt::Display,
    {
        let mut lines = spawn_stdin();
        let mut joined = std::pin::pin!(joined);
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else { break };
                    match self.on_line(line.trim()).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) => self.printer.error(&format!("{e:#}")),
                    }
                }
                res = &mut joined, if self.space.is_none() => match res {
                    Ok(space) => {
                        self.space = Some(space);
                        if self.meta.status == JoinStatus::Pending {
                            self.state.set_joined(0)?;
                            self.meta.status = JoinStatus::Joined;
                        }
                        self.refresh_members().await;
                    }
                    Err(e) => self.printer.error(&format!("joining space failed: {e}")),
                },
                ev = rx.next() => {
                    let Some(ev) = ev else { break };
                    if let Err(e) = self.on_event(ev).await {
                        self.printer.error(&format!("{e:#}"));
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        Ok(())
    }

    async fn on_line(&mut self, line: &str) -> Result<bool> {
        if let Some(member) = (!line.starts_with('/'))
            .then(|| self.asks.pop_front())
            .flatten()
        {
            if matches!(line.to_ascii_lowercase().as_str(), "y" | "yes") {
                self.add(member).await?;
            } else {
                self.printer
                    .info(&format!("declined {}", short_id(&member)));
            }
            return Ok(true);
        }
        match line {
            "" => {}
            "/quit" => return Ok(false),
            "/members" => {
                self.refresh_members().await;
                let ids: Vec<String> = self.members.iter().map(hex_id).collect();
                self.printer
                    .emit(json!({"event": "members", "members": ids}), || {
                        ids.join("\n")
                    });
            }
            "/status" => {
                let c = self.state.counts()?;
                self.printer.info(&format!(
                    "{} {} | {} members | ops ok={} skipped={} failed={}",
                    self.meta.role.as_str(),
                    self.meta.status.as_str(),
                    self.members.len(),
                    c.ok,
                    c.skipped,
                    c.failed
                ));
            }
            l if l.starts_with('/') => self.printer.error(&format!("unknown command {l}")),
            text => {
                let Some(space) = &self.space else {
                    self.printer
                        .error("not joined yet; messages are disabled until the creator adds you");
                    return Ok(true);
                };
                let body = ChatBody::new(text, now_ms())?;
                // The plaintext is stored when our own operation comes back
                // through the stream; spaces do not return its hash here.
                space.publish(body).await?;
            }
        }
        Ok(true)
    }

    async fn add(&mut self, member: NodeId) -> Result<()> {
        let Some(space) = &self.space else {
            bail!("space not ready");
        };
        space.add(key(&member)?, AccessLevel::Write).await?;
        tracing::info!(member = %short_id(&member), "added member");
        Ok(())
    }

    async fn refresh_members(&mut self) {
        let Some(space) = &self.space else { return };
        if let Ok(list) = space.members().await {
            let now: BTreeSet<NodeId> = list.iter().map(|(id, _)| *id.as_bytes()).collect();
            self.announce(now);
        }
    }

    /// Prints a `joined` event for every member not seen before.
    fn announce(&mut self, now: BTreeSet<NodeId>) {
        let fresh: Vec<NodeId> = now
            .iter()
            .filter(|m| !self.members.contains(*m))
            .copied()
            .collect();
        let count = now.len();
        self.members = now;
        for m in fresh {
            let who = if m == self.me {
                "you".into()
            } else {
                short_id(&m)
            };
            self.printer.emit(
                json!({"event": "joined", "member": hex_id(&m), "epoch": null, "members": count}),
                || format!("{who} joined ({count} members)"),
            );
        }
    }

    async fn on_event(&mut self, ev: StreamEvent<ChatBody>) -> Result<()> {
        match ev {
            StreamEvent::Processed { operation, .. } => {
                let hash = *operation.id().as_bytes();
                if self.state.outcome(&hash)?.is_some() {
                    operation.ack().await?;
                    return Ok(());
                }
                let author = *operation.author().as_bytes();
                let body = operation.message().clone();
                let msg = StoredMessage {
                    op_hash: hash,
                    author,
                    sent_at_ms: body.sent_at_ms,
                    op_ts: Some(operation.timestamp()),
                    text: body.text.clone(),
                };
                self.state.record(&hash, Outcome::Ok, None, Some(&msg))?;
                let mine = author == self.me;
                let who = if mine { "me".into() } else { short_id(&author) };
                self.printer.emit(
                    json!({"event": "message", "author": hex_id(&author), "text": body.text,
                           "op": hex::encode(hash), "mine": mine, "ts": operation.timestamp() / 1000}),
                    || format!("{who}: {}", body.text),
                );
                operation.ack().await?;
            }
            StreamEvent::Member(vk) => {
                let member = *vk.as_bytes();
                if self.meta.role != Role::Creator
                    || member == self.me
                    || self.members.contains(&member)
                    || self.asks.contains(&member)
                    || self.space.is_none()
                {
                    return Ok(());
                }
                self.printer.emit(
                    json!({"event": "join_request", "member": hex_id(&member)}),
                    || {
                        format!(
                            "join request from {}. Accept? [y/N]{}",
                            short_id(&member),
                            if self.opts.auto_accept { " (auto)" } else { "" }
                        )
                    },
                );
                if self.opts.auto_accept {
                    self.add(member).await?;
                } else {
                    self.asks.push_back(member);
                }
            }
            StreamEvent::Space { members, .. } => {
                let now: BTreeSet<NodeId> = members.keys().map(|k| *k.as_bytes()).collect();
                if now.contains(&self.me) {
                    self.announce(now);
                }
            }
            StreamEvent::SyncStarted {
                remote_node_id,
                incoming_operations,
                incoming_bytes,
                ..
            } => {
                let peer = *remote_node_id.as_bytes();
                if self.printer.json || self.printer.verbose {
                    self.printer.emit(
                        json!({"event": "sync_started", "peer": hex_id(&peer),
                               "incoming_ops": incoming_operations, "incoming_bytes": incoming_bytes}),
                        || format!("sync started with {}: {incoming_operations} ops", short_id(&peer)),
                    );
                }
            }
            StreamEvent::SyncEnded {
                remote_node_id,
                received_operations,
                sent_operations,
                error,
                ..
            } => {
                let peer = *remote_node_id.as_bytes();
                let error = error.map(|e| e.to_string());
                if self.printer.json || self.printer.verbose {
                    self.printer.emit(
                        json!({"event": "sync_ended", "peer": hex_id(&peer),
                               "received_ops": received_operations, "sent_ops": sent_operations,
                               "error": error}),
                        || {
                            format!(
                                "sync ended with {}: {received_operations} received",
                                short_id(&peer)
                            )
                        },
                    );
                }
            }
            StreamEvent::ReplayStarted { total_operations } if self.printer.verbose => {
                self.printer.info(&format!(
                    "replaying {total_operations} unacknowledged operations"
                ))
            }
            StreamEvent::ProcessingFailed { error, .. } => {
                tracing::warn!("processing failed: {error}");
                if self.printer.verbose {
                    self.printer
                        .info(&format!("p2panda: processing failed: {error}"));
                }
            }
            StreamEvent::DecodeFailed { error, .. } => {
                tracing::warn!("decode failed: {error}");
            }
            _ => {}
        }
        Ok(())
    }
}
