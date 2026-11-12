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
use p2panda::streams::{StreamEvent, StreamPublisher, StreamSubscription};
use p2panda::{AccessLevel, Hash, Node, RelayUrl, VerifyingKey};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::{StreamExt, StreamMap};
use triad_agent::AgentConfig;
use triad_agent::policy::Incoming;
use triad_core::envelope::{
    ChatBody, Invite, ProfileUpdate, RoomEvent, SEND_TEXT_BYTES, addresses_bot, normalize_nickname,
};
use triad_core::identity::{Identity, NodeId, hex_id, short_id};
use triad_core::long::{self, LongBody};
use triad_core::net::{DEFAULT_RELAY, P2PANDA_FILE};
use triad_core::state::{
    JoinStatus, Meta, Outcome, Role, State, StoredMessage, WaitingLong, channel_name, now_ms,
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
    /// Set your nickname in the channels opened by this session.
    #[arg(long, global = true, value_name = "NAME")]
    nickname: Option<String>,
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
    /// Create a channel (a new space) with my bot in it, print its invite,
    /// enter chat mode.
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
        /// Plain line-based output instead of the full-screen interface.
        #[arg(long, conflicts_with = "json")]
        plain: bool,
        /// Run without my bots: no personal bot, and no bot in channels I
        /// create or host.
        #[arg(long, env = "TRIAD_NO_BOT", value_parser = clap::builder::BoolishValueParser::new())]
        no_bot: bool,
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
        /// Plain line-based output instead of the full-screen interface.
        #[arg(long, conflicts_with = "json")]
        plain: bool,
        /// Run without my bots: no personal bot, and no bot in channels I
        /// create or host.
        #[arg(long, env = "TRIAD_NO_BOT", value_parser = clap::builder::BoolishValueParser::new())]
        no_bot: bool,
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
        /// Plain line-based output instead of the full-screen interface.
        #[arg(long, conflicts_with = "json")]
        plain: bool,
        /// Run without my bots: no personal bot, and no bot in channels I
        /// create or host.
        #[arg(long, env = "TRIAD_NO_BOT", value_parser = clap::builder::BoolishValueParser::new())]
        no_bot: bool,
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

struct Opts {
    nickname: Option<String>,
    auto_accept: bool,
    mode: Mode,
    verbose: bool,
}

/// How a chat session talks to its user.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// One JSON object per line (`--json`), the tested protocol.
    Json,
    /// Plain text lines.
    Plain,
    /// The full-screen terminal interface.
    Tui,
}

impl Mode {
    /// `--json` wins; then `--plain`, a pipe on stdin or stdout, or a build
    /// without the `tui` feature mean plain text; otherwise the interface.
    fn select(json: bool, plain: bool) -> Mode {
        use std::io::IsTerminal;
        if json {
            Mode::Json
        } else if plain
            || !cfg!(feature = "tui")
            || !std::io::stdin().is_terminal()
            || !std::io::stdout().is_terminal()
        {
            Mode::Plain
        } else {
            Mode::Tui
        }
    }
}

/// Whether a session runs this user's bots. They run unless `--no-bot` is
/// given; a configuration that cannot run them (no model key, say) is
/// reported in the session, which then runs as a plain chat.
enum BotStart {
    Off,
    On(AgentConfig),
    Unavailable(String),
}

impl BotStart {
    fn new(no_bot: bool, data_dir: &Path) -> Self {
        if no_bot {
            return BotStart::Off;
        }
        match AgentConfig::load(data_dir) {
            Ok(config) => BotStart::On(config),
            Err(e) => BotStart::Unavailable(e.to_string()),
        }
    }
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

fn message_sender(message: &StoredMessage, me: &NodeId, profile: Option<String>) -> String {
    if &message.author == me {
        "me".into()
    } else {
        profile
            .or_else(|| message.nickname.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| short_id(&message.author))
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
    let nickname = cli
        .nickname
        .as_deref()
        .map(|name| normalize_nickname(name).map(|name| name.unwrap_or_default().to_owned()))
        .transpose()?;
    match cli.cmd {
        Cmd::Whoami => println!("{}", Identity::load_or_create(&data_dir)?.node_hex()),
        Cmd::Create {
            name,
            relay,
            auto_accept,
            json,
            plain,
            no_bot,
        } => {
            let opts = Opts {
                nickname,
                auto_accept,
                mode: Mode::select(json, plain),
                verbose,
            };
            let bots = BotStart::new(no_bot, &data_dir);
            rt.block_on(chat(&data_dir, Start::Create { name, relay }, opts, bots))?;
        }
        Cmd::Join {
            invite,
            name,
            auto_accept,
            json,
            plain,
            no_bot,
        } => {
            let invite = Invite::parse(&invite)?;
            let opts = Opts {
                nickname,
                auto_accept,
                mode: Mode::select(json, plain),
                verbose,
            };
            let bots = BotStart::new(no_bot, &data_dir);
            rt.block_on(chat(&data_dir, Start::Join { invite, name }, opts, bots))?;
        }
        Cmd::Chat {
            channel,
            auto_accept,
            json,
            plain,
            no_bot,
        } => {
            let opts = Opts {
                nickname,
                auto_accept,
                mode: Mode::select(json, plain),
                verbose,
            };
            let bots = BotStart::new(no_bot, &data_dir);
            rt.block_on(chat(&data_dir, Start::Resume { channel }, opts, bots))?;
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
                let profiles = state.profiles(&g.topic)?;
                for m in state.history(&g.topic, limit)? {
                    let key = (m.op_ts.unwrap_or(m.sent_at_ms * 1000), m.op_hash);
                    let profile = profiles.get(&m.author).cloned();
                    lines.push((key, &g.name, m, profile));
                }
            }
            lines.sort_by_key(|(key, ..)| *key);
            if let Some(limit) = limit
                && lines.len() > limit
            {
                lines.drain(..lines.len() - limit);
            }
            for (_, name, m, profile) in lines {
                let base = message_sender(&m, &me, profile);
                let who = match (m.author == me, m.bot) {
                    (true, true) => "my bot".into(),
                    (false, true) => format!("{base} (bot)"),
                    _ => base,
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

/// Where a session's output goes. Every event is a JSON value with a text
/// form; the sink decides which is used.
enum Sink {
    Text,
    Json,
    /// The terminal interface's thread, which gets both.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    Tui(mpsc::UnboundedSender<(serde_json::Value, String)>),
}

struct Printer {
    sink: Sink,
    verbose: bool,
}

impl Printer {
    fn emit(&self, value: serde_json::Value, text: impl FnOnce() -> String) {
        match &self.sink {
            Sink::Json => println!("{value}"),
            Sink::Text => println!("{}", text()),
            Sink::Tui(tx) => {
                let _ = tx.send((value, text()));
            }
        }
    }

    fn json(&self) -> bool {
        matches!(self.sink, Sink::Json)
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
    subs: StreamMap<Topic, SpaceSubscription<RoomEvent>>,
    joining: JoinSet<(Topic, Result<Space<RoomEvent>, String>)>,
    /// Bot turns running off the event loop.
    jobs: JoinSet<bots::JobDone>,
    /// Display lines from bot turns (tool calls).
    bot_events: mpsc::UnboundedReceiver<bots::BotEvent>,
    /// Long-message ciphertext, one plain stream per space, keyed by the
    /// space's topic.
    longs: StreamMap<Topic, StreamSubscription<LongBody>>,
}

impl Streams {
    fn wait_joined(&mut self, topic: Topic, pending: PendingSpace<RoomEvent>) {
        self.joining
            .spawn(async move { (topic, pending.joined().await.map_err(|e| e.to_string())) });
    }
}

struct Channel {
    meta: Meta,
    /// Set once this node has been added to the space.
    space: Option<Space<RoomEvent>>,
    members: BTreeMap<NodeId, AccessLevel>,
    /// True once this node has seen itself in the member list.
    seen_self: bool,
}

async fn chat(data_dir: &Path, start: Start, opts: Opts, bots: BotStart) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let me = identity.node_id();
    let state = State::open(data_dir)?;
    let groups = state.groups()?;

    // This node's relay for new channels and invites: the one its first
    // channel uses, so a restart with no arguments listens where peers expect.
    let home_relay = match (&start, groups.first()) {
        (Start::Resume { .. }, None) => match &bots {
            BotStart::On(_) => DEFAULT_RELAY.to_string(),
            BotStart::Off => bail!("no channels in this data directory; run `create` or `join`"),
            BotStart::Unavailable(why) => bail!(
                "no channels in this data directory, and no bot to talk to ({why}); \
                 run `create` or `join`"
            ),
        },
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
        data_dir: data_dir.to_path_buf(),
        dm,
        bots: None,
        bot_events: bot_events_tx,
        me,
        identity,
        node,
        state,
        home_relay,
        channels: BTreeMap::new(),
        focus: [0; 32],
        printer: Printer {
            sink: match opts.mode {
                Mode::Json => Sink::Json,
                // The interface comes in a later step; plain until then.
                Mode::Plain | Mode::Tui => Sink::Text,
            },
            verbose: opts.verbose,
        },
        opts,
        asks: VecDeque::new(),
        long_pubs: BTreeMap::new(),
        held_long: None,
    };
    let mut streams = Streams {
        subs: StreamMap::new(),
        joining: JoinSet::new(),
        jobs: JoinSet::new(),
        bot_events: bot_events_rx,
        longs: StreamMap::new(),
    };
    let first = groups.first().map(|g| g.topic);
    for meta in groups {
        session.open(meta, &mut streams).await?;
    }
    let result = async {
        match &bots {
            BotStart::On(config) => {
                if let Err(e) = session.start_bots(config, &mut streams).await {
                    session.bots_unavailable(&format!("{e:#}"));
                }
            }
            BotStart::Unavailable(why) => session.bots_unavailable(why),
            BotStart::Off => {}
        }
        session.focus = match start {
            Start::Create { name, relay } => {
                let topic = session
                    .create_channel(name.as_deref(), relay, &mut streams)
                    .await?;
                // A new channel gets my bot, unless the session runs
                // without one.
                if session.bots.is_some() {
                    session.bot_on(topic, &mut streams).await?;
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
            // With bots and no channel named, chat opens on the personal
            // bot's DM.
            Start::Resume { channel: None } => match (session.bots.is_some(), session.dm) {
                (true, Some(dm)) => dm,
                _ => first.expect("checked above"),
            },
        };
        session.ready()?;
        session.resume_jobs(&mut streams).await;
        session.event_loop(&mut streams, spawn_stdin()).await
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
    data_dir: PathBuf,
    me: NodeId,
    identity: Identity,
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
    /// The personal bot's DM, once created (bots on or off this session).
    dm: Option<Topic>,
    /// The bots, unless off this session (`--no-bot`, or no model).
    bots: Option<bots::Bots>,
    /// Where bot turns send display lines.
    bot_events: mpsc::UnboundedSender<bots::BotEvent>,
    /// Publishers for long-message ciphertext, by space topic.
    long_pubs: BTreeMap<Topic, StreamPublisher<LongBody>>,
    /// Test hook `/debug-long-hold`: ciphertext not yet published.
    held_long: Option<(Topic, LongBody)>,
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

    fn joined_space(&self, topic: &Topic) -> Result<&Space<RoomEvent>> {
        let channel = self.channel(topic)?;
        channel
            .space
            .as_ref()
            .with_context(|| format!("not a member of #{} yet", channel.meta.name))
    }

    /// Subscribes to the plain stream that carries a space's long-message
    /// ciphertext ([`triad_core::long`]).
    async fn open_long(&mut self, topic: Topic, streams: &mut Streams) -> Result<()> {
        let (publisher, rx) = self
            .node
            .stream::<LongBody>(Hash::from(long::stream_topic(&topic)))
            .await
            .context("subscribing to a channel's long-message stream")?;
        self.long_pubs.insert(topic, publisher);
        streams.longs.insert(topic, rx);
        Ok(())
    }

    /// Subscribes to a channel already recorded in `state.sqlite`.
    async fn open(&mut self, meta: Meta, streams: &mut Streams) -> Result<()> {
        let topic = meta.topic;
        let (pending, rx) = self
            .node
            .space::<RoomEvent>(Hash::from(topic))
            .await
            .with_context(|| format!("subscribing to #{}", meta.name))?;
        streams.subs.insert(topic, rx);
        streams.wait_joined(topic, pending);
        self.open_long(topic, streams).await?;
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
        let (space, rx) = self.node.create_space::<RoomEvent>().await?;
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
        self.open_long(topic, streams).await?;
        self.channels.insert(
            topic,
            Channel {
                meta,
                space: Some(space),
                members: BTreeMap::from([(self.me, AccessLevel::Manage)]),
                seen_self: true,
            },
        );
        self.publish_nickname(topic).await?;
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

    async fn event_loop(
        &mut self,
        streams: &mut Streams,
        mut lines: mpsc::Receiver<String>,
    ) -> Result<()> {
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
                Some((topic, ev)) = streams.longs.next() => {
                    if let Err(e) = self.on_long_event(topic, ev, streams).await {
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

    async fn on_joined(&mut self, topic: Topic, space: Space<RoomEvent>) -> Result<()> {
        let Some(channel) = self.channels.get_mut(&topic) else {
            return Ok(());
        };
        channel.space = Some(space);
        if channel.meta.status == JoinStatus::Pending {
            self.state.set_joined(&topic)?;
            channel.meta.status = JoinStatus::Joined;
        }
        self.refresh_members(topic).await;
        self.publish_nickname(topic).await
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
                let (name, no_bot) = match rest.split_whitespace().collect::<Vec<_>>()[..] {
                    [name] => (name, false),
                    [name, "--no-bot"] | ["--no-bot", name] => (name, true),
                    _ => bail!("usage: /create <name> [--no-bot]"),
                };
                let topic = self.create_channel(Some(name), None, streams).await?;
                self.added(topic)?;
                if self.bots.is_some() && !no_bot {
                    self.bot_on(topic, streams).await?;
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
                if !self.on_bot_command(other, rest, streams).await? {
                    self.printer.error(&format!("unknown command {other}"))
                }
            }
        }
        Ok(true)
    }

    async fn send(&mut self, topic: Topic, text: &str) -> Result<()> {
        let channel = self.channel(&topic)?;
        if channel.space.is_none() {
            self.printer.error(&format!(
                "not in #{} yet; messages are disabled until a manager adds you",
                channel.meta.name
            ));
            return Ok(());
        }
        let ask = addresses_bot(text);
        if ask && !self.has_bot(&topic) {
            bail!("#{} has no bot", channel.meta.name);
        }
        // The plaintext is stored when our own operation comes back through
        // the stream; spaces do not return its hash here.
        self.publish_chat(topic, ChatBody::new(text, now_ms())?.asking(ask))
            .await
    }

    /// Publishes a chat message into a space. Text over `SEND_TEXT_BYTES`
    /// goes as a long message: the ciphertext first, on the plain stream,
    /// then the space message that holds its key.
    async fn publish_chat(&self, topic: Topic, body: ChatBody) -> Result<()> {
        let space = self.joined_space(&topic)?;
        let body = if body.text.len() > SEND_TEXT_BYTES {
            let (long, payload) = long::seal(&body.text)?;
            self.long_pubs
                .get(&topic)
                .context("no long-message stream for this channel")?
                .publish(payload)
                .await?;
            body.into_long(long)
        } else {
            body
        };
        space.publish(RoomEvent::Chat(body)).await?;
        Ok(())
    }

    async fn publish_nickname(&self, topic: Topic) -> Result<()> {
        if let Some(nickname) = &self.opts.nickname
            && self.state.profile(&topic, &self.me)?.as_deref() != Some(nickname)
        {
            let revision = self.state.next_profile_revision(&topic)?;
            let profile = ProfileUpdate::sign(&self.identity, &topic, revision, nickname)?;
            self.joined_space(&topic)?
                .publish(RoomEvent::Profile(profile))
                .await?;
        }
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
    /// How a message's author is shown. `who` is their name as
    /// [`message_sender`] gives it.
    fn label(&self, topic: &Topic, who: String, claimed: bool, accepted: bool) -> String {
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

    /// Records, shows and (for a bot) queues one chat message whose full
    /// text is in `body`.
    async fn on_chat(
        &mut self,
        topic: Topic,
        hash: [u8; 32],
        author: NodeId,
        op_ts: u64,
        body: ChatBody,
        streams: &mut Streams,
    ) -> Result<()> {
        let name = self.name(&topic).to_string();
        let msg = StoredMessage {
            op_hash: hash,
            author,
            sent_at_ms: body.sent_at_ms,
            op_ts: Some(op_ts),
            text: body.text.clone(),
            bot: body.bot,
            nickname: body.nickname().map(str::to_owned),
        };
        let incoming = Incoming {
            author,
            bot: body.bot,
            ask: body.ask,
            ctl: body.ctl.is_some(),
            at_ms: op_ts / 1000,
        };
        let job = self.bot_job(topic, hash, &incoming).await?;
        self.state
            .record_with_job(&topic, &hash, Outcome::Ok, None, Some(&msg), job.as_ref())?;
        if let Some(ctl) = &body.ctl {
            self.on_control(topic, author, ctl).await?;
        }
        let mine = author == self.me;
        let profile = self.state.profile(&topic, &author)?;
        let base = message_sender(&msg, &self.me, profile.clone());
        let nickname = profile
            .or_else(|| msg.nickname.clone())
            .filter(|name| !name.is_empty());
        let shown_as_bot = body.bot && self.bot_accepted(&topic, &author);
        let who = self.label(&topic, base, body.bot, shown_as_bot);
        let mut value = json!({"event": "message", "channel": name, "author": hex_id(&author),
               "text": body.text, "nickname": nickname, "op": hex::encode(hash), "mine": mine,
               "ts": op_ts / 1000,
               "bot": shown_as_bot, "ask": body.ask, "long": body.long.is_some()});
        if body.bot && !shown_as_bot {
            value["bot_claim"] = json!("not accepted");
        }
        self.printer
            .emit(value, || format!("[#{name}] {who}: {}", body.text));
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
        Ok(())
    }

    /// Opens a long message with its ciphertext and handles it as
    /// [`Self::on_chat`] does. One that cannot be opened is recorded as
    /// failed.
    #[allow(clippy::too_many_arguments)]
    async fn on_long_chat(
        &mut self,
        topic: Topic,
        hash: [u8; 32],
        author: NodeId,
        op_ts: u64,
        body: ChatBody,
        ciphertext: &[u8],
        streams: &mut Streams,
    ) -> Result<()> {
        let opened = body
            .long
            .as_ref()
            .context("not a long message")
            .and_then(|r| long::open(r, ciphertext));
        match opened {
            Ok(text) => {
                self.on_chat(topic, hash, author, op_ts, body.with_text(text), streams)
                    .await
            }
            Err(e) => {
                self.state.record(
                    &topic,
                    &hash,
                    Outcome::Failed,
                    Some(&format!("{e:#}")),
                    None,
                )?;
                self.printer.error(&format!(
                    "a long message in #{} from {} could not be read: {e:#}",
                    self.name(&topic),
                    short_id(&author)
                ));
                Ok(())
            }
        }
    }

    /// Ciphertext of a long message, from the plain stream beside a space.
    /// Stored, then any space message waiting for it is finished.
    async fn on_long_event(
        &mut self,
        topic: Topic,
        ev: StreamEvent<LongBody>,
        streams: &mut Streams,
    ) -> Result<()> {
        let StreamEvent::Processed { operation, .. } = ev else {
            return Ok(());
        };
        let ct = &operation.message().ciphertext;
        // Anyone who knows the topic can publish here; the hash in a
        // member's space message decides what is read. Oversized bodies are
        // not stored.
        if ct.len() <= long::MAX_CIPHERTEXT_BYTES {
            let hash = long::hash_of(ct);
            self.state.put_long_body(&topic, &hash, ct)?;
            for w in self.state.take_waiting(&hash)? {
                if self.state.outcome(&w.op_hash)?.is_some() {
                    continue;
                }
                let body = ChatBody::decode(&w.body)?;
                self.on_long_chat(w.topic, w.op_hash, w.author, w.op_ts, body, ct, streams)
                    .await?;
            }
        }
        operation.ack().await?;
        Ok(())
    }

    async fn on_event(
        &mut self,
        topic: Topic,
        ev: StreamEvent<RoomEvent>,
        streams: &mut Streams,
    ) -> Result<()> {
        let name = self.name(&topic).to_string();
        match ev {
            StreamEvent::Processed { operation, .. } => {
                let hash = *operation.id().as_bytes();
                let body = match operation.message() {
                    RoomEvent::Profile(profile) => {
                        if let Ok((author, revision, nickname)) = profile.verify(&topic) {
                            self.state
                                .record_profile(&topic, &author, revision, nickname)?;
                        }
                        operation.ack().await?;
                        return Ok(());
                    }
                    RoomEvent::Chat(body) => body,
                };
                if self.state.outcome(&hash)?.is_some() {
                    operation.ack().await?;
                    return Ok(());
                }
                let author = *operation.author().as_bytes();
                let op_ts = operation.timestamp();
                let body = body.clone();
                match &body.long {
                    None => {
                        self.on_chat(topic, hash, author, op_ts, body, streams)
                            .await?
                    }
                    Some(r) => match self.state.long_body(&r.hash)? {
                        Some(ct) => {
                            self.on_long_chat(topic, hash, author, op_ts, body, &ct, streams)
                                .await?
                        }
                        None => {
                            // The ciphertext has not arrived on the plain
                            // stream yet; finished in `on_long_event`.
                            tracing::debug!(op = %hex::encode(&hash[..4]), "long message waits for its body");
                            self.state.wait_long(&WaitingLong {
                                op_hash: hash,
                                topic,
                                hash: r.hash,
                                author,
                                op_ts,
                                body: body.encode()?,
                            })?;
                        }
                    },
                }
                operation.ack().await?;
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
                if self.printer.json() || self.printer.verbose {
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
                if self.printer.json() || self.printer.verbose {
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
