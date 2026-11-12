//! `triad`: chat on p2panda's built-in encrypted spaces. p2panda does
//! group key agreement and encryption, signs and syncs the logs, and dials
//! peers through iroh. The app keeps names, invites and readable history.
//!
//! Join flow: the joiner subscribes to the space, which announces its key
//! bundle on the space topic. The creator sees that as a join request and adds
//! the member with `Space::add`. Any member with `Manage` access handles join
//! requests the same way.
//!
//! One data directory (one identity, one p2panda node) holds any number of
//! spaces, each a named channel with its own members and history. Chat mode
//! opens all of them at once. Plain lines go to the focused channel, and
//! `/create`, `/join`, `/switch`, `/channels`, `/msg` and `/invite` manage
//! the rest, as in an IRC client.
//!
//! Membership commands in chat mode, for the offline-membership tests:
//! `/add <node>`, `/remove <node>`, `/promote <node>` (to `Manage`). They act
//! on the focused channel.

mod bots;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use p2panda::node::AckPolicy;
use p2panda::spaces::{PendingSpace, Space, SpaceSubscription};
use p2panda::streams::StreamEvent;
use p2panda::{AccessLevel, Hash, Node, RelayUrl, VerifyingKey};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::{StreamExt, StreamMap};
use triad_agent::AgentConfig;
use triad_agent::policy::Incoming;
use triad_core::envelope::{ChatBody, Invite, SEND_TEXT_BYTES};
use triad_core::identity::{Identity, NodeId, hex_id, short_id};
use triad_core::net::{DEFAULT_RELAY, P2PANDA_FILE};
use triad_core::state::{
    JoinStatus, Meta, Outcome, Role, State, StoredMessage, channel_name, now_ms,
};

#[derive(Parser)]
#[command(
    name = "triad",
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
    /// Create a channel (a new space), print its invite, enter chat mode.
    Create {
        /// Name of the new channel (default `general`).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        #[arg(long)]
        auto_accept: bool,
        #[arg(long)]
        json: bool,
        /// Run my bots in this session (see HARNESS-PLAN.md).
        #[arg(long)]
        agent: bool,
    },
    /// Join a channel from an invite, wait to be added, enter chat mode.
    Join {
        invite: String,
        /// Local name for the channel (default: the name in the invite).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long)]
        auto_accept: bool,
        #[arg(long)]
        json: bool,
        /// Run my bots in this session.
        #[arg(long)]
        agent: bool,
    },
    /// Open every channel in chat mode.
    Chat {
        /// Channel to focus first (default: the first one added).
        #[arg(long, value_name = "NAME")]
        channel: Option<String>,
        #[arg(long)]
        auto_accept: bool,
        #[arg(long)]
        json: bool,
        /// Run my bots in this session.
        #[arg(long)]
        agent: bool,
    },
    /// List the channels in this data directory.
    Channels,
    /// Print an invite to a channel this node has joined.
    Invite {
        /// Channel to invite to (default: the first one added).
        #[arg(long, value_name = "NAME")]
        channel: Option<String>,
    },
    /// Print stored messages, oldest first. Without `--channel`, every
    /// channel's messages in one timeline, each line tagged with its channel.
    History {
        #[arg(long, value_name = "NAME")]
        channel: Option<String>,
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Print role, join status and operation counts for every channel.
    Status,
    /// Scan p2panda.sqlite for the text. Exit 0 if absent, 1 if found.
    Inspect {
        #[arg(long, value_name = "TEXT")]
        grep: String,
    },
    /// Raw iroh connectivity check, below p2panda.
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

#[derive(Clone, Copy)]
struct Opts {
    auto_accept: bool,
    json: bool,
    verbose: bool,
}

/// The bot configuration, read from the environment when `--agent` is
/// given. A bad configuration is a usage error: exit code 2.
fn agent_config(agent: bool) -> Result<Option<AgentConfig>, ExitCode> {
    if !agent {
        return Ok(None);
    }
    AgentConfig::from_env().map(Some).map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::from(2)
    })
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
        home.join("Library/Application Support/triad")
    } else {
        home.join(".local/share/triad")
    })
}

fn run_cli(cli: Cli) -> Result<ExitCode> {
    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => default_data_dir()?,
    };
    // The removed OpenMLS version of `triad` used the same default directory.
    // Its group is not a p2panda space, so it cannot be opened as a channel.
    if !matches!(cli.cmd, Cmd::Inspect { .. } | Cmd::Probe { .. })
        && data_dir.join("mls.sqlite").exists()
    {
        bail!(
            "{} was written by the OpenMLS version of triad, which this version cannot read; \
             move it away or pass a fresh --data-dir",
            data_dir.display()
        );
    }
    std::fs::create_dir_all(&data_dir)?;
    if !matches!(cli.cmd, Cmd::Inspect { .. } | Cmd::Probe { .. }) {
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
            name,
            relay,
            auto_accept,
            json,
            agent,
        } => {
            let opts = Opts {
                auto_accept,
                json,
                verbose,
            };
            let agent = match agent_config(agent) {
                Ok(a) => a,
                Err(code) => return Ok(code),
            };
            rt.block_on(chat(&data_dir, Start::Create { name, relay }, opts, agent))?;
        }
        Cmd::Join {
            invite,
            name,
            auto_accept,
            json,
            agent,
        } => {
            let invite = Invite::parse(&invite)?;
            let opts = Opts {
                auto_accept,
                json,
                verbose,
            };
            let agent = match agent_config(agent) {
                Ok(a) => a,
                Err(code) => return Ok(code),
            };
            rt.block_on(chat(&data_dir, Start::Join { invite, name }, opts, agent))?;
        }
        Cmd::Chat {
            channel,
            auto_accept,
            json,
            agent,
        } => {
            let opts = Opts {
                auto_accept,
                json,
                verbose,
            };
            let agent = match agent_config(agent) {
                Ok(a) => a,
                Err(code) => return Ok(code),
            };
            rt.block_on(chat(&data_dir, Start::Resume { channel }, opts, agent))?;
        }
        Cmd::Channels => {
            for g in State::open(&data_dir)?.groups()? {
                println!(
                    "#{} {} {} {}",
                    g.name,
                    g.role.as_str(),
                    g.status.as_str(),
                    hex::encode(g.topic)
                );
            }
        }
        Cmd::Invite { channel } => {
            let state = State::open(&data_dir)?;
            let me = Identity::load_or_create(&data_dir)?.node_id();
            let meta = pick(&state, channel.as_deref())?;
            if meta.status != JoinStatus::Joined {
                bail!("not a member of #{} yet", meta.name);
            }
            if state.dm_topic()? == Some(meta.topic) {
                bail!("#{} is your bot DM; it never gains a member", meta.name);
            }
            println!("{}", invite_to(&meta, &me)?);
        }
        Cmd::History { channel, limit } => {
            let state = State::open(&data_dir)?;
            let me = Identity::load_or_create(&data_dir)?.node_id();
            let groups = match &channel {
                Some(_) => vec![pick(&state, channel.as_deref())?],
                None => state.groups()?,
            };
            let mut lines = Vec::new();
            for g in &groups {
                for m in state.history(&g.topic, limit)? {
                    let key = (m.op_ts.unwrap_or(m.sent_at_ms * 1000), m.op_hash);
                    lines.push((key, g.name.as_str(), m));
                }
            }
            lines.sort_by_key(|(key, ..)| *key);
            if let Some(limit) = limit
                && lines.len() > limit
            {
                lines.drain(..lines.len() - limit);
            }
            for (_, name, m) in lines {
                let who = match (m.author == me, m.bot) {
                    (true, false) => "me".into(),
                    (true, true) => "my bot".into(),
                    (false, false) => short_id(&m.author),
                    (false, true) => format!("{} (bot)", short_id(&m.author)),
                };
                if channel.is_some() {
                    println!("{}: {}", who, m.text);
                } else {
                    println!("#{name} {}: {}", who, m.text);
                }
            }
        }
        Cmd::Status => {
            let state = State::open(&data_dir)?;
            let me = Identity::load_or_create(&data_dir)?;
            println!("node    {}", me.node_hex());
            let groups = state.groups()?;
            if groups.is_empty() {
                println!("status  none");
            }
            for meta in groups {
                let c = state.counts(&meta.topic)?;
                println!("channel #{}", meta.name);
                println!("role    {}", meta.role.as_str());
                println!("status  {}", meta.status.as_str());
                println!("space   {}", hex::encode(meta.topic));
                println!(
                    "ops     ok={} skipped={} failed={}",
                    c.ok, c.skipped, c.failed
                );
            }
        }
        Cmd::Inspect { grep } => return inspect(&data_dir, &grep),
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
    rt.shutdown_timeout(Duration::from_millis(500));
    Ok(ExitCode::SUCCESS)
}

/// The named channel, or the first one when no name is given.
fn pick(state: &State, channel: Option<&str>) -> Result<Meta> {
    match channel {
        Some(name) => state
            .group_named(name)?
            .with_context(|| format!("no channel named {name}")),
        None => state
            .meta()?
            .context("no channels in this data directory; run `create` or `join`"),
    }
}

/// An invite that names this node as the way into the channel. Any member
/// can hand one out; a member with `Manage` access has to be online to admit
/// the joiner.
fn invite_to(meta: &Meta, me: &NodeId) -> Result<String> {
    Invite::new(meta.topic, *me, &meta.relay)
        .with_name(&meta.name)
        .render()
}

fn inspect(data_dir: &Path, needle: &str) -> Result<ExitCode> {
    if needle.is_empty() {
        bail!("--grep needs non-empty text");
    }
    let mut found = 0;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = data_dir.join(format!("{P2PANDA_FILE}{suffix}"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if !suffix.is_empty() && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        found += bytes
            .windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count();
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
    Create {
        name: Option<String>,
        relay: Option<String>,
    },
    Join {
        invite: Invite,
        name: Option<String>,
    },
    Resume {
        channel: Option<String>,
    },
}

/// Name given to `create` when none is passed.
const DEFAULT_CHANNEL: &str = "general";

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

fn parse_node(text: &str) -> Result<NodeId> {
    let bytes = hex::decode(text.trim()).context("node ID must be hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node ID must be 32 bytes"))
}

/// Whether a typed line addresses the channel's bot: `@bot` as a word
/// anywhere in it. Only the sender's client reads the text; bots read `ask`.
fn addresses_bot(text: &str) -> bool {
    text.split_whitespace().any(|w| {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '@')
            .eq_ignore_ascii_case("@bot")
    })
}

fn access_name(a: AccessLevel) -> &'static str {
    match a {
        AccessLevel::Pull => "pull",
        AccessLevel::Read => "read",
        AccessLevel::Write => "write",
        AccessLevel::Manage => "manage",
    }
}

fn key(id: &NodeId) -> Result<VerifyingKey> {
    VerifyingKey::from_bytes(id).context("invalid node ID")
}

fn relay_url(relay: &str) -> Result<RelayUrl> {
    relay
        .parse()
        .with_context(|| format!("invalid relay URL {relay}"))
}

type Topic = [u8; 32];

/// The live p2panda side of every channel: one event stream per space, and
/// one task per space this node is still waiting to be let into.
struct Streams {
    subs: StreamMap<Topic, SpaceSubscription<ChatBody>>,
    joining: JoinSet<(Topic, Result<Space<ChatBody>, String>)>,
    /// Bot turns running off the event loop.
    jobs: JoinSet<bots::JobDone>,
    /// Display lines from bot turns (tool calls).
    bot_events: mpsc::UnboundedReceiver<bots::BotEvent>,
}

impl Streams {
    fn wait_joined(&mut self, topic: Topic, pending: PendingSpace<ChatBody>) {
        self.joining
            .spawn(async move { (topic, pending.joined().await.map_err(|e| e.to_string())) });
    }
}

struct Channel {
    meta: Meta,
    /// Set once this node has been added to the space.
    space: Option<Space<ChatBody>>,
    members: BTreeMap<NodeId, AccessLevel>,
    /// True once this node has seen itself in the member list.
    seen_self: bool,
}

async fn chat(data_dir: &Path, start: Start, opts: Opts, agent: Option<AgentConfig>) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let me = identity.node_id();
    let state = State::open(data_dir)?;
    let groups = state.groups()?;

    // This node's relay for new channels and invites: the one its first
    // channel uses, so a restart with no arguments listens where peers expect.
    let home_relay = match (&start, groups.first()) {
        (Start::Resume { .. }, None) if agent.is_none() => {
            bail!("no channels in this data directory; run `create` or `join`")
        }
        (Start::Resume { .. }, None) => DEFAULT_RELAY.to_string(),
        (_, Some(first)) => first.relay.clone(),
        (Start::Create { relay, .. }, None) => {
            relay.clone().unwrap_or_else(|| DEFAULT_RELAY.to_string())
        }
        (Start::Join { invite, .. }, None) => invite.relay.clone(),
    };
    let mut relays: BTreeSet<&str> = groups.iter().map(|g| g.relay.as_str()).collect();
    relays.insert(&home_relay);
    match &start {
        Start::Create { relay: Some(r), .. } => {
            relays.insert(r);
        }
        Start::Join { invite, .. } => {
            relays.insert(&invite.relay);
        }
        _ => {}
    }

    let db = data_dir.join(P2PANDA_FILE);
    let mut builder = p2panda::builder()
        .credentials(identity.p2panda_credentials())
        .database_url(&format!("sqlite://{}?mode=rwc", db.display()))
        .ack_policy(AckPolicy::Explicit);
    for relay in relays {
        builder = builder.relay_url(relay_url(relay)?);
    }
    // Every channel joined through someone else is reached through them.
    for g in groups.iter().filter(|g| g.creator != me) {
        builder = builder.bootstrap(key(&g.creator)?, relay_url(&g.relay)?);
    }
    let node = builder.spawn().await.context("spawning p2panda node")?;

    let dm = state.dm_topic()?;
    let (bot_events_tx, bot_events_rx) = mpsc::unbounded_channel();
    let mut session = Session {
        dm,
        bots: None,
        bot_events: bot_events_tx,
        me,
        node,
        state,
        home_relay,
        channels: BTreeMap::new(),
        focus: [0; 32],
        printer: Printer {
            json: opts.json,
            verbose: opts.verbose,
        },
        opts,
        asks: VecDeque::new(),
    };
    let mut streams = Streams {
        subs: StreamMap::new(),
        joining: JoinSet::new(),
        jobs: JoinSet::new(),
        bot_events: bot_events_rx,
    };
    let first = groups.first().map(|g| g.topic);
    for meta in groups {
        session.open(meta, &mut streams).await?;
    }
    let result = async {
        if let Some(config) = &agent {
            session.start_bots(config, data_dir, &mut streams).await?;
        }
        session.focus = match start {
            Start::Create { name, relay } => {
                let topic = session
                    .create_channel(name.as_deref(), relay, &mut streams)
                    .await?;
                // Creating a channel while running with --agent offers my
                // bot to it.
                if session.bots.is_some() {
                    session.bot_on(topic).await?;
                }
                topic
            }
            Start::Join { invite, name } => {
                session
                    .join_channel(invite, name.as_deref(), &mut streams)
                    .await?
            }
            Start::Resume {
                channel: Some(name),
            } => session.resolve(&name)?,
            // With --agent, chat opens on the personal bot's DM.
            Start::Resume { channel: None } => match (session.bots.is_some(), session.dm) {
                (true, Some(dm)) => dm,
                _ => first.expect("checked above"),
            },
        };
        session.ready()?;
        session.resume_jobs(&mut streams).await;
        session.event_loop(&mut streams).await
    }
    .await;
    for (_, channel) in std::mem::take(&mut session.channels) {
        if let Some(space) = channel.space {
            let _ = space.close().await;
        }
    }
    drop(streams);
    drop(session.node);
    result
}

struct Session {
    me: NodeId,
    node: Node,
    state: State,
    home_relay: String,
    channels: BTreeMap<Topic, Channel>,
    /// The channel plain lines and membership commands go to.
    focus: Topic,
    printer: Printer,
    opts: Opts,
    /// Join requests waiting for an answer, oldest first.
    asks: VecDeque<(Topic, NodeId)>,
    /// The personal bot's DM, once created (with or without `--agent`).
    dm: Option<Topic>,
    /// The bots, when running with `--agent`.
    bots: Option<bots::Bots>,
    /// Where bot turns send display lines.
    bot_events: mpsc::UnboundedSender<bots::BotEvent>,
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
    fn channel(&self, topic: &Topic) -> Result<&Channel> {
        self.channels.get(topic).context("unknown channel")
    }

    fn name(&self, topic: &Topic) -> &str {
        self.channels.get(topic).map_or("?", |c| &c.meta.name)
    }

    /// The topic of the channel with this name (a leading `#` is accepted).
    fn resolve(&self, name: &str) -> Result<Topic> {
        let name = channel_name(name)?;
        self.channels
            .values()
            .find(|c| c.meta.name == name)
            .map(|c| c.meta.topic)
            .with_context(|| format!("no channel named #{name}; see /channels"))
    }

    fn joined_space(&self, topic: &Topic) -> Result<&Space<ChatBody>> {
        let channel = self.channel(topic)?;
        channel
            .space
            .as_ref()
            .with_context(|| format!("not a member of #{} yet", channel.meta.name))
    }

    /// Subscribes to a channel already recorded in `state.sqlite`.
    async fn open(&mut self, meta: Meta, streams: &mut Streams) -> Result<()> {
        let topic = meta.topic;
        let (pending, rx) = self
            .node
            .space::<ChatBody>(Hash::from(topic))
            .await
            .with_context(|| format!("subscribing to #{}", meta.name))?;
        streams.subs.insert(topic, rx);
        streams.wait_joined(topic, pending);
        self.channels.insert(
            topic,
            Channel {
                meta,
                space: None,
                members: BTreeMap::new(),
                seen_self: false,
            },
        );
        Ok(())
    }

    /// Creates a space with this node as its only member and manager.
    async fn create_channel(
        &mut self,
        name: Option<&str>,
        relay: Option<String>,
        streams: &mut Streams,
    ) -> Result<Topic> {
        let name = channel_name(name.unwrap_or(DEFAULT_CHANNEL))?;
        if self.channels.values().any(|c| c.meta.name == name) {
            bail!("a channel named #{name} already exists; pick another name");
        }
        let (space, rx) = self.node.create_space::<ChatBody>().await?;
        let topic = *space.id().as_bytes();
        let meta = Meta {
            name,
            topic,
            creator: self.me,
            relay: relay.unwrap_or_else(|| self.home_relay.clone()),
            role: Role::Creator,
            status: JoinStatus::Joined,
        };
        self.state.insert_group(&meta)?;
        tracing::info!(channel = %meta.name, space = %hex::encode(&topic[..4]), "created channel");
        streams.subs.insert(topic, rx);
        self.channels.insert(
            topic,
            Channel {
                meta,
                space: Some(space),
                members: BTreeMap::from([(self.me, AccessLevel::Manage)]),
                seen_self: true,
            },
        );
        Ok(topic)
    }

    /// Subscribes to the invite's space, which announces this node's key
    /// bundle there, and waits for a manager to add it.
    async fn join_channel(
        &mut self,
        invite: Invite,
        name: Option<&str>,
        streams: &mut Streams,
    ) -> Result<Topic> {
        if let Some(c) = self.channels.get(&invite.topic) {
            bail!("already in this channel, as #{}", c.meta.name);
        }
        let taken = |n: &str| self.channels.values().any(|c| c.meta.name == n);
        let name = match name {
            Some(n) => {
                let n = channel_name(n)?;
                if taken(&n) {
                    bail!("a channel named #{n} already exists; pick another name");
                }
                n
            }
            None => {
                // The invite's name is a suggestion from someone else: fall
                // back to the space ID if it is unusable or already taken.
                let id = hex::encode(&invite.topic[..4]);
                let suggested = invite.name.as_deref().and_then(|n| channel_name(n).ok());
                match suggested {
                    Some(n) if !taken(&n) => n,
                    Some(n) => {
                        let n: String = n.chars().take(23).collect();
                        format!("{n}-{id}")
                    }
                    None => id,
                }
            }
        };
        if taken(&name) {
            bail!("a channel named #{name} already exists; pass a name");
        }
        if invite.creator != self.me {
            self.node
                .insert_bootstrap(key(&invite.creator)?, relay_url(&invite.relay)?)
                .await
                .context("adding the inviter as a bootstrap node")?;
        }
        let meta = Meta {
            name,
            topic: invite.topic,
            creator: invite.creator,
            relay: invite.relay,
            role: Role::Member,
            status: JoinStatus::Pending,
        };
        self.state.insert_group(&meta)?;
        tracing::info!(channel = %meta.name, space = %hex::encode(&meta.topic[..4]), "joining channel");
        let topic = meta.topic;
        self.open(meta, streams).await?;
        Ok(topic)
    }

    fn channel_json(&self, c: &Channel) -> Result<serde_json::Value> {
        let invite = (c.meta.status == JoinStatus::Joined && !self.is_dm(&c.meta.topic))
            .then(|| invite_to(&c.meta, &self.me))
            .transpose()?;
        Ok(json!({
            "channel": c.meta.name, "group": hex::encode(c.meta.topic),
            "role": c.meta.role.as_str(), "status": c.meta.status.as_str(),
            "invite": invite, "focused": c.meta.topic == self.focus,
        }))
    }

    /// Channels sorted by name.
    fn sorted(&self) -> Vec<&Channel> {
        let mut list: Vec<&Channel> = self.channels.values().collect();
        list.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
        list
    }

    fn channel_lines(&self) -> String {
        self.sorted()
            .iter()
            .map(|c| {
                format!(
                    "{} #{} ({}, {})",
                    if c.meta.topic == self.focus { "*" } else { " " },
                    c.meta.name,
                    c.meta.role.as_str(),
                    c.meta.status.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Announces the session: the focused channel's details at the top
    /// level, as before channels existed, plus the list of all channels.
    fn ready(&self) -> Result<()> {
        let focused = self.channel(&self.focus)?;
        let mut value = self.channel_json(focused)?;
        let all = self
            .sorted()
            .into_iter()
            .map(|c| self.channel_json(c))
            .collect::<Result<Vec<_>>>()?;
        let fields = value.as_object_mut().expect("object");
        fields.insert("event".into(), json!("ready"));
        fields.insert("node".into(), json!(hex_id(&self.me)));
        fields.insert("channels".into(), json!(all));
        fields.remove("focused");
        let invite = value["invite"].as_str().map(str::to_string);
        self.printer.emit(value, || {
            let mut s = format!("node    {}\n{}", short_id(&self.me), self.channel_lines());
            if let Some(i) = &invite {
                s.push_str(&format!("\ninvite  {i}"));
            }
            s.push_str("\nType a message, /channels, /switch <name>, or /quit.");
            s
        });
        Ok(())
    }

    /// Reports a channel added while chat mode is running, and focuses it.
    fn added(&mut self, topic: Topic) -> Result<()> {
        self.focus = topic;
        let c = self.channel(&topic)?;
        let mut value = self.channel_json(c)?;
        value["event"] = json!("channel");
        let invite = value["invite"].as_str().map(str::to_string);
        self.printer.emit(value, || match invite {
            Some(i) => format!("now in #{}\ninvite  {i}", c.meta.name),
            None => format!(
                "waiting to be let into #{}; messages there are disabled until then",
                c.meta.name
            ),
        });
        Ok(())
    }

    async fn event_loop(&mut self, streams: &mut Streams) -> Result<()> {
        let mut lines = spawn_stdin();
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else { break };
                    match self.on_line(line.trim(), streams).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) => self.printer.error(&format!("{e:#}")),
                    }
                }
                Some(res) = streams.joining.join_next() => match res {
                    Ok((topic, Ok(space))) => self.on_joined(topic, space).await?,
                    Ok((topic, Err(e))) => self
                        .printer
                        .error(&format!("joining #{} failed: {e}", self.name(&topic))),
                    Err(e) => self.printer.error(&format!("joining a channel failed: {e}")),
                },
                Some((topic, ev)) = streams.subs.next() => {
                    if let Err(e) = self.on_event(topic, ev, streams).await {
                        self.printer.error(&format!("{e:#}"));
                    }
                }
                Some(done) = streams.jobs.join_next() => match done {
                    Ok(done) => self.on_job_done(done, streams).await,
                    Err(e) => self.printer.error(&format!("a bot turn failed: {e}")),
                },
                Some((value, text)) = streams.bot_events.recv() => {
                    self.printer.emit(value, || text);
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        Ok(())
    }

    async fn on_joined(&mut self, topic: Topic, space: Space<ChatBody>) -> Result<()> {
        let Some(channel) = self.channels.get_mut(&topic) else {
            return Ok(());
        };
        channel.space = Some(space);
        if channel.meta.status == JoinStatus::Pending {
            self.state.set_joined(&topic)?;
            channel.meta.status = JoinStatus::Joined;
        }
        self.refresh_members(topic).await;
        Ok(())
    }

    async fn on_line(&mut self, line: &str, streams: &mut Streams) -> Result<bool> {
        if !line.starts_with('/') {
            if let Some((topic, _)) = self.asks.front().copied() {
                // A key bundle can arrive just before the membership change
                // that adds its owner (another manager added them). Drop
                // requests from nodes that are members by now, so this line
                // is not taken as an answer to a stale prompt.
                self.refresh_members(topic).await;
                let channels = &self.channels;
                self.asks
                    .retain(|(t, m)| !channels.get(t).is_some_and(|c| c.members.contains_key(m)));
            }
            if let Some((topic, member)) = self.asks.pop_front() {
                if matches!(line.to_ascii_lowercase().as_str(), "y" | "yes") {
                    self.add(topic, member).await?;
                } else {
                    self.printer
                        .info(&format!("declined {}", short_id(&member)));
                }
                return Ok(true);
            }
            if !line.is_empty() {
                self.send(self.focus, line).await?;
            }
            return Ok(true);
        }
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((cmd, rest)) => (cmd, rest.trim()),
            None => (line, ""),
        };
        let focus = self.focus;
        match cmd {
            "/quit" => return Ok(false),
            "/channels" | "/list" => {
                let all = self
                    .sorted()
                    .into_iter()
                    .map(|c| self.channel_json(c))
                    .collect::<Result<Vec<_>>>()?;
                self.printer
                    .emit(json!({"event": "channels", "channels": all}), || {
                        self.channel_lines()
                    });
            }
            "/switch" | "/s" => {
                self.focus = self.resolve(rest)?;
                let name = self.name(&self.focus);
                self.printer
                    .emit(json!({"event": "switched", "channel": name}), || {
                        format!("now talking in #{name}")
                    });
            }
            "/create" => {
                if rest.is_empty() {
                    bail!("usage: /create <name>");
                }
                let topic = self.create_channel(Some(rest), None, streams).await?;
                self.added(topic)?;
                if self.bots.is_some() {
                    self.bot_on(topic).await?;
                }
            }
            "/join" => {
                let mut words = rest.split_whitespace();
                let invite = words.next().context("usage: /join <invite> [name]")?;
                let invite = Invite::parse(invite)?;
                let topic = self.join_channel(invite, words.next(), streams).await?;
                self.added(topic)?;
            }
            "/invite" => {
                let topic = if rest.is_empty() {
                    focus
                } else {
                    self.resolve(rest)?
                };
                let c = self.channel(&topic)?;
                if c.meta.status != JoinStatus::Joined {
                    bail!("not a member of #{} yet", c.meta.name);
                }
                if self.is_dm(&topic) {
                    bail!("#{} is your bot DM; it never gains a member", c.meta.name);
                }
                let invite = invite_to(&c.meta, &self.me)?;
                self.printer.emit(
                    json!({"event": "invite", "channel": c.meta.name, "invite": invite}),
                    || format!("invite to #{}: {invite}", c.meta.name),
                );
            }
            "/msg" => {
                let (name, text) = rest
                    .split_once(char::is_whitespace)
                    .context("usage: /msg <channel> <text>")?;
                let topic = self.resolve(name)?;
                self.send(topic, text.trim()).await?;
            }
            "/members" => {
                self.refresh_members(focus).await;
                let c = self.channel(&focus)?;
                let ids: Vec<String> = c.members.keys().map(hex_id).collect();
                let access: serde_json::Map<String, serde_json::Value> = c
                    .members
                    .iter()
                    .map(|(id, a)| (hex_id(id), json!(access_name(*a))))
                    .collect();
                self.printer.emit(
                    json!({"event": "members", "channel": c.meta.name,
                           "members": ids, "access": access}),
                    || {
                        c.members
                            .iter()
                            .map(|(id, a)| format!("{} {}", hex_id(id), access_name(*a)))
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                );
            }
            "/add" | "/promote" if self.is_dm(&focus) => {
                bail!(
                    "#{} is your bot DM; it never gains a member",
                    self.name(&focus)
                )
            }
            "/add" => self.add(focus, parse_node(rest)?).await?,
            "/remove" => {
                let member = parse_node(rest)?;
                self.joined_space(&focus)?.remove(key(&member)?).await?;
                tracing::info!(member = %short_id(&member), "removed member");
            }
            "/promote" => {
                let member = parse_node(rest)?;
                self.joined_space(&focus)?
                    .promote(key(&member)?, AccessLevel::Manage)
                    .await?;
                tracing::info!(member = %short_id(&member), "promoted member");
            }
            "/status" => {
                let c = self.channel(&focus)?;
                let counts = self.state.counts(&focus)?;
                self.printer.info(&format!(
                    "#{} {} {} | {} members | ops ok={} skipped={} failed={}",
                    c.meta.name,
                    c.meta.role.as_str(),
                    c.meta.status.as_str(),
                    c.members.len(),
                    counts.ok,
                    counts.skipped,
                    counts.failed
                ));
            }
            other => {
                if !self.on_bot_command(other, rest).await? {
                    self.printer.error(&format!("unknown command {other}"))
                }
            }
        }
        Ok(true)
    }

    async fn send(&mut self, topic: Topic, text: &str) -> Result<()> {
        let channel = self.channel(&topic)?;
        let Some(space) = &channel.space else {
            self.printer.error(&format!(
                "not in #{} yet; messages are disabled until a manager adds you",
                channel.meta.name
            ));
            return Ok(());
        };
        if text.len() > SEND_TEXT_BYTES {
            bail!(
                "message is {} bytes; spaces can carry at most {SEND_TEXT_BYTES} today",
                text.len()
            );
        }
        let ask = addresses_bot(text);
        if ask && !self.has_bot(&topic) {
            bail!("#{} has no bot", channel.meta.name);
        }
        let body = ChatBody::new(text, now_ms())?.asking(ask);
        // The plaintext is stored when our own operation comes back through
        // the stream; spaces do not return its hash here.
        space.publish(body).await?;
        Ok(())
    }

    async fn add(&mut self, topic: Topic, member: NodeId) -> Result<()> {
        self.joined_space(&topic)?
            .add(key(&member)?, AccessLevel::Write)
            .await?;
        tracing::info!(member = %short_id(&member), channel = %self.name(&topic), "added member");
        Ok(())
    }

    async fn refresh_members(&mut self, topic: Topic) {
        let Some(space) = self.channels.get(&topic).and_then(|c| c.space.as_ref()) else {
            return;
        };
        if let Ok(list) = space.members().await {
            let now: BTreeMap<NodeId, AccessLevel> =
                list.iter().map(|(id, a)| (*id.as_bytes(), *a)).collect();
            self.announce(topic, now);
        }
    }

    /// Prints `joined` for every member not seen before, `promoted` for an
    /// access change, and `removed` for every member that disappeared,
    /// including this node.
    fn announce(&mut self, topic: Topic, now: BTreeMap<NodeId, AccessLevel>) {
        let me = self.me;
        let Some(channel) = self.channels.get_mut(&topic) else {
            return;
        };
        if !channel.seen_self && !now.contains_key(&me) {
            // Not in yet: nothing to compare against.
            return;
        }
        let name = channel.meta.name.as_str();
        let count = now.len();
        let who = |m: &NodeId| {
            if *m == me {
                "you".to_string()
            } else {
                short_id(m)
            }
        };
        let mut lines = Vec::new();
        for (m, a) in &now {
            match channel.members.get(m) {
                None => lines.push((
                    json!({"event": "joined", "channel": name, "member": hex_id(m), "members": count, "access": access_name(*a)}),
                    format!("[#{name}] {} joined ({count} members)", who(m)),
                )),
                Some(old) if old != a => lines.push((
                    json!({"event": "promoted", "channel": name, "member": hex_id(m),
                           "access": access_name(*a)}),
                    format!("[#{name}] {} is now {}", who(m), access_name(*a)),
                )),
                _ => {}
            }
        }
        for m in channel.members.keys().filter(|m| !now.contains_key(*m)) {
            lines.push((
                json!({"event": "removed", "channel": name, "member": hex_id(m),
                       "members": count}),
                format!("[#{name}] {} removed ({count} members)", who(m)),
            ));
        }
        channel.seen_self = now.contains_key(&me);
        channel.members = now;
        for (value, text) in lines {
            self.printer.emit(value, || text);
        }
    }

    /// Who a message is shown as: `me`, a short node ID, `my bot:` in the
    /// DM, `bot` in a channel, or a note that a bot claim was not accepted.
    fn label(&self, topic: &Topic, author: &NodeId, claimed: bool, accepted: bool) -> String {
        let who = if *author == self.me {
            "me".to_string()
        } else {
            short_id(author)
        };
        match (claimed, accepted) {
            (true, true) if self.is_dm(topic) => "my bot".to_string(),
            (true, true) => "bot".to_string(),
            (true, false) => format!("{who} (bot claim not accepted)"),
            _ => who,
        }
    }

    /// Whether this node may add members to the channel (`Manage` access).
    async fn can_manage(&self, topic: &Topic) -> bool {
        let Some(space) = self.channels.get(topic).and_then(|c| c.space.as_ref()) else {
            return false;
        };
        match space.members().await {
            Ok(list) => list
                .iter()
                .any(|(id, a)| *id.as_bytes() == self.me && *a == AccessLevel::Manage),
            Err(_) => false,
        }
    }

    async fn on_event(
        &mut self,
        topic: Topic,
        ev: StreamEvent<ChatBody>,
        streams: &mut Streams,
    ) -> Result<()> {
        let name = self.name(&topic).to_string();
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
                    bot: body.bot,
                };
                let incoming = Incoming {
                    author,
                    bot: body.bot,
                    ask: body.ask,
                    ctl: body.ctl.is_some(),
                    at_ms: operation.timestamp() / 1000,
                };
                let job = self.bot_job(topic, hash, &incoming).await?;
                self.state.record_with_job(
                    &topic,
                    &hash,
                    Outcome::Ok,
                    None,
                    Some(&msg),
                    job.as_ref(),
                )?;
                if let Some(ctl) = &body.ctl {
                    self.on_control(topic, author, ctl).await?;
                }
                let mine = author == self.me;
                let shown_as_bot = body.bot && self.bot_accepted(&topic, &author);
                let who = self.label(&topic, &author, body.bot, shown_as_bot);
                let mut value = json!({"event": "message", "channel": name, "author": hex_id(&author),
                       "text": body.text, "op": hex::encode(hash), "mine": mine,
                       "ts": operation.timestamp() / 1000,
                       "bot": shown_as_bot, "ask": body.ask});
                if body.bot && !shown_as_bot {
                    value["bot_claim"] = json!("not accepted");
                }
                self.printer
                    .emit(value, || format!("[#{name}] {who}: {}", body.text));
                operation.ack().await?;
                if let Some(job) = job {
                    self.emit_job(
                        &topic,
                        &hash,
                        &job.asker,
                        job.state,
                        job.reason.as_deref(),
                        None,
                    );
                    if job.state == triad_core::state::JobState::Queued {
                        self.pump(topic, streams).await;
                    }
                }
            }
            StreamEvent::Member(vk) if self.is_dm(&topic) => {
                // The bot DM never gains a member: decline without asking,
                // even under --auto-accept.
                let member = *vk.as_bytes();
                if member != self.me {
                    self.printer.emit(
                        json!({"event": "join_declined", "channel": name, "member": hex_id(&member),
                               "reason": "bot DM"}),
                        || {
                            format!(
                                "declined {} for #{name}: your bot DM never gains a member",
                                short_id(&member)
                            )
                        },
                    );
                }
            }
            StreamEvent::Member(vk) => {
                let member = *vk.as_bytes();
                if member == self.me
                    || self.channel(&topic)?.members.contains_key(&member)
                    || self.asks.contains(&(topic, member))
                    || !self.can_manage(&topic).await
                {
                    return Ok(());
                }
                self.printer.emit(
                    json!({"event": "join_request", "channel": name, "member": hex_id(&member)}),
                    || {
                        format!(
                            "join request for #{name} from {}. Accept? [y/N]{}",
                            short_id(&member),
                            if self.opts.auto_accept { " (auto)" } else { "" }
                        )
                    },
                );
                if self.opts.auto_accept {
                    self.add(topic, member).await?;
                } else {
                    self.asks.push_back((topic, member));
                }
            }
            StreamEvent::Space { members, .. } => {
                // `Ejected` arrives with an empty member list.
                let now: BTreeMap<NodeId, AccessLevel> =
                    members.iter().map(|(k, a)| (*k.as_bytes(), *a)).collect();
                self.announce(topic, now);
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
                        json!({"event": "sync_started", "channel": name, "peer": hex_id(&peer),
                               "incoming_ops": incoming_operations, "incoming_bytes": incoming_bytes}),
                        || format!("[#{name}] sync started with {}: {incoming_operations} ops", short_id(&peer)),
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
                        json!({"event": "sync_ended", "channel": name, "peer": hex_id(&peer),
                               "received_ops": received_operations, "sent_ops": sent_operations,
                               "error": error}),
                        || {
                            format!(
                                "[#{name}] sync ended with {}: {received_operations} received",
                                short_id(&peer)
                            )
                        },
                    );
                }
            }
            StreamEvent::ReplayStarted { total_operations } if self.printer.verbose => {
                self.printer.info(&format!(
                    "#{name}: replaying {total_operations} unacknowledged operations"
                ))
            }
            StreamEvent::ProcessingFailed { error, .. } => {
                tracing::warn!(channel = %name, "processing failed: {error}");
                if self.printer.verbose {
                    self.printer
                        .info(&format!("p2panda: #{name}: processing failed: {error}"));
                }
            }
            StreamEvent::DecodeFailed { error, .. } => {
                tracing::warn!(channel = %name, "decode failed: {error}");
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_bot_is_a_word_anywhere() {
        assert!(addresses_bot("@bot add this"));
        assert!(addresses_bot("hey @bot, add this"));
        assert!(addresses_bot("add this @Bot"));
        assert!(!addresses_bot("email bot@example.com"));
        assert!(!addresses_bot("@bottle"));
        assert!(!addresses_bot("no address"));
    }
}
