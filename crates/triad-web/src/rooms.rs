//! One identity's room: a p2panda space, its history and profiles, and,
//! for the room's creator, the bot. The bot runs here as that identity,
//! following the same policy as the CLI's channel bot: answers only
//! `@bot` lines, refuses by the same limits, and is accepted by members as
//! the first `BotOn` from a member with `Manage` access.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::pending,
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use p2panda::{
    AccessLevel, Hash, Node, SpaceEvent, VerifyingKey,
    node::AckPolicy,
    spaces::{PendingSpace, Space, SpaceSubscription},
    streams::StreamEvent,
};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_stream::StreamExt;
use triad_agent::{
    Agent, AgentConfig,
    context::{self, ContextLimits, HistoryLine, Kind, Turn},
    policy::{self, ChannelView, Decision, Incoming, ToolSet},
    tools::{NoAudit, offered, servers},
};
use triad_core::{
    envelope::{
        ChatBody, Control, Invite, ProfileUpdate, RoomEvent, SEND_TEXT_BYTES, split_text,
        truncate_text,
    },
    identity::{Identity, NodeId, short_id},
    net::{DEFAULT_RELAY, P2PANDA_FILE},
    state::{
        BotRow, Job, JobState, JoinStatus, Meta, NewJob, Outcome, Role, State, StoredMessage,
        channel_name, now_ms,
    },
};

pub enum Start {
    /// A new room, with this identity's bot unless `bot` is false.
    Create {
        bot: bool,
    },
    Join(Invite),
    Resume,
}

pub struct Account {
    pub identity: Identity,
    pub session: Mutex<Session>,
    dir: PathBuf,
}

pub struct Session {
    pub view: View,
    resume_on_load: bool,
    send: Option<mpsc::Sender<Send>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct View {
    node: String,
    status: &'static str,
    group: String,
    invite: String,
    error: String,
    running: bool,
    messages: Vec<Message>,
    profiles: BTreeMap<String, String>,
    /// This identity created the room, so it may add or remove its bot.
    creator: bool,
    /// The room's accepted bot, once announced.
    bot: Option<BotView>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct BotView {
    host: String,
    mine: bool,
    enabled: bool,
    provider: String,
}

impl BotView {
    fn new(row: &BotRow, me: NodeId) -> Self {
        Self {
            host: hex::encode(row.host),
            mine: row.host == me,
            enabled: row.enabled,
            provider: row.provider.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Message {
    author: String,
    nickname: Option<String>,
    text: String,
    mine: bool,
    /// Sent by the room's accepted bot.
    bot: bool,
}

pub enum Outgoing {
    Chat(ChatBody),
    Nickname(String),
    /// Add (true) or remove this identity's bot. Creator only.
    Bot(bool),
}

/// The bot this identity runs in its room, once started.
struct BotHost {
    agent: Option<Arc<Agent>>,
    /// A turn is running; jobs wait in the queue.
    busy: bool,
    done: mpsc::UnboundedSender<JobDone>,
}

/// A finished turn, back on the room's loop.
struct JobDone {
    op_hash: [u8; 32],
    result: Result<String, String>,
}

struct Send {
    body: Outgoing,
    result: oneshot::Sender<Result<()>>,
}

impl Account {
    pub fn load(dir: PathBuf) -> Result<Self> {
        let identity = Identity::load_or_create(&dir)?;
        let state = State::open(&dir)?;
        let meta = state.meta()?;
        let mut view = View {
            node: identity.node_hex(),
            status: "none",
            group: String::new(),
            invite: String::new(),
            error: String::new(),
            running: false,
            messages: Vec::new(),
            profiles: BTreeMap::new(),
            creator: false,
            bot: None,
        };
        if let Some(meta) = &meta {
            view.room(meta)?;
            let bot = state.bot(&meta.topic)?;
            view.bot = bot.as_ref().map(|b| BotView::new(b, identity.node_id()));
            let host = bot.map(|b| b.host);
            view.profiles = state
                .profiles(&meta.topic)?
                .into_iter()
                .map(|(author, nickname)| (hex::encode(author), nickname))
                .collect();
            view.messages = state
                .history(&meta.topic, None)?
                .into_iter()
                .map(|message| Message::from_stored(message, identity.node_id(), host))
                .collect();
            view.status = "disconnected";
        }
        Ok(Self {
            identity,
            session: Mutex::new(Session {
                view,
                resume_on_load: meta.is_some(),
                send: None,
            }),
            dir,
        })
    }

    pub async fn resume_on_load(self: &Arc<Self>) {
        let mut session = self.session.lock().await;
        if session.resume_on_load && !session.view.running {
            self.launch(&mut session, Start::Resume);
        }
    }

    pub async fn start(self: &Arc<Self>, start: Start) -> Result<()> {
        let mut session = self.session.lock().await;
        if session.view.running {
            bail!("this identity already has an active room");
        }
        if matches!(start, Start::Resume) == session.view.group.is_empty() {
            bail!("this identity can only open one room");
        }
        self.launch(&mut session, start);
        Ok(())
    }

    /// Whether `@bot` reaches a bot in this room.
    pub async fn has_bot(&self) -> bool {
        let session = self.session.lock().await;
        session.view.bot.as_ref().is_some_and(|b| b.enabled)
    }

    pub async fn send(&self, body: Outgoing) -> Result<()> {
        let session = self.session.lock().await;
        if session.view.status != "joined" {
            bail!("wait until the room is connected");
        }
        let queue = session
            .send
            .as_ref()
            .ok_or_else(|| anyhow!("room is disconnected"))?
            .clone();
        drop(session);
        let (result, receive) = oneshot::channel();
        queue
            .send(Send { body, result })
            .await
            .map_err(|_| anyhow!("room is disconnected"))?;
        receive.await?
    }

    fn launch(self: &Arc<Self>, session: &mut Session, start: Start) {
        let (send, commands) = mpsc::channel(32);
        session.send = Some(send);
        session.resume_on_load = false;
        session.view.running = true;
        session.view.status = "starting";
        session.view.error.clear();
        let account = self.clone();
        tokio::spawn(async move {
            let result = account.run(start, commands).await;
            let mut session = account.session.lock().await;
            session.send = None;
            session.view.running = false;
            if session.view.status != "removed" {
                session.view.status = "disconnected";
            }
            if let Err(error) = result {
                session.view.error = format!("{error:#}");
            }
        });
    }

    async fn run(&self, start: Start, commands: mpsc::Receiver<Send>) -> Result<()> {
        let mut store = State::open(&self.dir)?;
        let me = self.identity.node_id();
        let bot_on_start = matches!(start, Start::Create { bot: true });
        let (relay, bootstrap) = match &start {
            Start::Create { .. } => (DEFAULT_RELAY.to_owned(), None),
            Start::Join(invite) => (invite.relay.clone(), Some(invite.creator)),
            Start::Resume => {
                let meta = store.meta()?.ok_or_else(|| anyhow!("no saved room"))?;
                (
                    meta.relay,
                    (meta.role == Role::Member).then_some(meta.creator),
                )
            }
        };
        let url: p2panda::RelayUrl = relay.parse()?;
        let mut builder = p2panda::builder()
            .credentials(self.identity.p2panda_credentials())
            .database_url(&format!(
                "sqlite://{}?mode=rwc",
                self.dir.join(P2PANDA_FILE).display()
            ))
            .ack_policy(AckPolicy::Explicit)
            .relay_url(url.clone());
        if let Some(creator) = bootstrap {
            builder = builder.bootstrap(VerifyingKey::from_bytes(&creator)?, url);
        }
        let node = builder.spawn().await?;
        let room = open_room(&node, &mut store, start, me, relay).await?;
        {
            let mut session = self.session.lock().await;
            session.view.room(&room.meta)?;
            session.view.status = if room.space.is_some() {
                "joined"
            } else {
                "pending"
            };
        }
        self.run_room(&mut store, room, commands, bot_on_start)
            .await
    }

    async fn run_room(
        &self,
        store: &mut State,
        room: Room,
        mut commands: mpsc::Receiver<Send>,
        bot_on_start: bool,
    ) -> Result<()> {
        let Room {
            meta,
            mut events,
            mut space,
            joining,
        } = room;
        let joined = async {
            match joining {
                Some(joining) => joining.joined().await,
                None => pending().await,
            }
        };
        tokio::pin!(joined);
        let mut waiting = space.is_none();
        let me = self.identity.node_id();
        let (done, mut finished) = mpsc::unbounded_channel();
        let mut host = BotHost {
            agent: None,
            busy: false,
            done,
        };
        // A new room gets the bot; a room whose bot this identity runs gets
        // it back. Both wait until the space is open.
        let hosting = matches!(store.bot(&meta.topic)?, Some(b) if b.enabled && b.host == me);
        let mut bot_pending = bot_on_start || hosting;
        loop {
            if bot_pending && space.is_some() {
                bot_pending = false;
                let started = if bot_on_start {
                    self.set_bot(true, space.as_ref(), store, &meta, &mut host)
                        .await
                } else {
                    self.ensure_agent(&mut host, store).await.map(|_| ())
                };
                match started {
                    Ok(()) => self.pump(&mut host, space.as_ref(), store, &meta).await?,
                    Err(error) => {
                        self.session.lock().await.view.error = format!("bot off: {error:#}");
                    }
                }
            }
            tokio::select! {
                result = &mut joined, if waiting => {
                    space = Some(result?);
                    waiting = false;
                    store.set_joined(&meta.topic)?;
                    self.session.lock().await.view.status = "joined";
                }
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()) };
                    let result = match command.body {
                        Outgoing::Bot(on) => {
                            self.set_bot(on, space.as_ref(), store, &meta, &mut host).await
                        }
                        body => publish(space.as_ref(), store, &meta.topic, &self.identity, body).await,
                    };
                    let _ = command.result.send(result);
                }
                Some(done) = finished.recv() => {
                    host.busy = false;
                    self.finish(done, space.as_ref(), store).await;
                    self.pump(&mut host, space.as_ref(), store, &meta).await?;
                }
                event = events.next() => {
                    match event {
                        Some(StreamEvent::Processed { operation, .. }) => {
                            let hash = *operation.id().as_bytes();
                            let author = *operation.author().as_bytes();
                            match operation.message() {
                                RoomEvent::Chat(body) if store.outcome(&hash)?.is_none() => {
                                    let message = StoredMessage {
                                        op_hash: hash,
                                        author,
                                        sent_at_ms: body.sent_at_ms,
                                        op_ts: Some(operation.timestamp()),
                                        text: body.text.clone(),
                                        bot: body.bot,
                                        nickname: body.nickname().map(str::to_owned),
                                    };
                                    let incoming = Incoming {
                                        author,
                                        bot: body.bot,
                                        ask: body.ask,
                                        ctl: body.ctl.is_some(),
                                        at_ms: operation.timestamp() / 1000,
                                    };
                                    let job = self.bot_job(&host, space.as_ref(), store, &meta.topic, &hash, &incoming).await?;
                                    store.record_with_job(&meta.topic, &hash, Outcome::Ok, None, Some(&message), job.as_ref())?;
                                    if let Some(ctl) = &body.ctl {
                                        self.on_control(space.as_ref(), store, &meta.topic, author, ctl).await?;
                                    }
                                    let bot_host = store.bot(&meta.topic)?.map(|b| b.host);
                                    self.session.lock().await.view.messages.push(Message::from_stored(message, me, bot_host));
                                    if job.is_some_and(|j| j.state == JobState::Queued) {
                                        self.pump(&mut host, space.as_ref(), store, &meta).await?;
                                    }
                                }
                                RoomEvent::Profile(profile) => {
                                    if let Ok((author, revision, nickname)) = profile.verify(&meta.topic)
                                        && store.record_profile(&meta.topic, &author, revision, nickname)?
                                    {
                                        self.session.lock().await.view.profiles.insert(hex::encode(author), nickname.to_owned());
                                    }
                                }
                                _ => {}
                            }
                            operation.ack().await?;
                        }
                        Some(StreamEvent::Member(member)) if meta.role == Role::Creator => {
                            if let Some(space) = &space
                                && !space.members().await?.iter().any(|(key, _)| *key == member)
                                && let Err(error) = space.add(member, AccessLevel::Write).await
                            {
                                self.session.lock().await.view.error = error.to_string();
                            }
                        }
                        Some(StreamEvent::Space { inner: SpaceEvent::Ejected { .. }, .. }) => {
                            self.session.lock().await.view.status = "removed";
                            return Ok(());
                        }
                        Some(_) => {}
                        None => bail!("room event stream closed"),
                    }
                }
            }
        }
    }
}

impl Account {
    /// Starts the bot's brain and tools, once.
    async fn ensure_agent(&self, host: &mut BotHost, store: &mut State) -> Result<Arc<Agent>> {
        if let Some(agent) = &host.agent {
            return Ok(agent.clone());
        }
        let config = AgentConfig::load(&self.dir).map_err(|e| anyhow!("{e}"))?;
        let agent = Arc::new(Agent::start(&config, &self.dir).await?);
        // A turn cut off by a restart is never retried.
        store.abandon_started()?;
        host.agent = Some(agent.clone());
        Ok(agent)
    }

    /// Adds or removes this identity's bot, as the room's creator, and
    /// announces it to members.
    async fn set_bot(
        &self,
        on: bool,
        space: Option<&Space<RoomEvent>>,
        store: &mut State,
        meta: &Meta,
        host: &mut BotHost,
    ) -> Result<()> {
        if meta.role != Role::Creator {
            bail!("only the room's creator can add its bot");
        }
        let space = space.ok_or_else(|| anyhow!("room is not connected"))?;
        let body = if on {
            let agent = self.ensure_agent(host, store).await?;
            let provider = agent.provider();
            let servers = agent.servers();
            let text = format!(
                "This channel has a bot, run by me. It sends the channel's messages to {provider} and can use: {}.",
                if servers.is_empty() {
                    "no tools".to_string()
                } else {
                    servers.join(", ")
                }
            );
            ChatBody::new(&truncate_text(&text, SEND_TEXT_BYTES), now_ms())?
                .with_ctl(Control::BotOn { provider, servers })
        } else {
            ChatBody::new("This channel's bot is off.", now_ms())?.with_ctl(Control::BotOff)
        };
        space.publish(RoomEvent::Chat(body)).await?.await?;
        Ok(())
    }

    /// Accepts the first `BotOn` from a member with `Manage` access, and
    /// from then on announcements only from that same author.
    async fn on_control(
        &self,
        space: Option<&Space<RoomEvent>>,
        store: &mut State,
        topic: &[u8; 32],
        author: NodeId,
        ctl: &Control,
    ) -> Result<()> {
        let current = store.bot(topic)?;
        let accepted = match &current {
            Some(b) => b.host == author,
            None => {
                matches!(ctl, Control::BotOn { .. })
                    && members(space).await.get(&author) == Some(&policy::Access::Manage)
            }
        };
        if !accepted {
            return Ok(());
        }
        let (enabled, provider, servers) = match ctl {
            Control::BotOn { provider, servers } => (true, provider.clone(), servers.clone()),
            Control::BotOff => (
                false,
                current
                    .as_ref()
                    .map(|b| b.provider.clone())
                    .unwrap_or_default(),
                current
                    .as_ref()
                    .map(|b| b.servers.clone())
                    .unwrap_or_default(),
            ),
        };
        let row = BotRow {
            topic: *topic,
            host: author,
            provider,
            servers,
            enabled,
            changed_at_ms: now_ms(),
        };
        store.set_bot(&row)?;
        self.session.lock().await.view.bot = Some(BotView::new(&row, self.identity.node_id()));
        Ok(())
    }

    /// The job for a message about to be recorded, if this identity runs
    /// the room's bot and the policy answers or refuses it.
    async fn bot_job(
        &self,
        host: &BotHost,
        space: Option<&Space<RoomEvent>>,
        store: &mut State,
        topic: &[u8; 32],
        hash: &[u8; 32],
        msg: &Incoming,
    ) -> Result<Option<NewJob>> {
        let me = self.identity.node_id();
        let Some(agent) = &host.agent else {
            return Ok(None);
        };
        if !msg.ask || msg.bot || msg.ctl {
            return Ok(None);
        }
        if !matches!(store.bot(topic)?, Some(b) if b.enabled && b.host == me) {
            return Ok(None);
        }
        let now = now_ms();
        let members = members(space).await;
        let grants = store.grants(topic)?;
        let blocked = store.blocked()?;
        let c = store.answer_counts(topic, &msg.author, now)?;
        let view = ChannelView {
            host: me,
            bot_enabled: true,
            members: &members,
            grants: &grants,
            blocked: &blocked,
            handled: store.job(hash)?.is_some(),
            counts: policy::Counts {
                asker_last_hour: c.asker_last_hour,
                channel_last_day: c.channel_last_day,
                channel_last_minute: c.channel_last_minute,
            },
        };
        Ok(
            match policy::decide_channel(msg, &view, &agent.limits, now) {
                Decision::Ignore => None,
                Decision::Refuse(r) => Some(NewJob {
                    asker: msg.author,
                    state: JobState::Refused,
                    reason: Some(r.as_str().into()),
                    tools: String::new(),
                }),
                Decision::Answer(tools) => Some(NewJob {
                    asker: msg.author,
                    state: JobState::Queued,
                    reason: None,
                    tools: tools.render(),
                }),
            },
        )
    }

    /// Starts the room's next queued job, unless a turn is running.
    async fn pump(
        &self,
        host: &mut BotHost,
        space: Option<&Space<RoomEvent>>,
        store: &mut State,
        meta: &Meta,
    ) -> Result<()> {
        loop {
            let Some(agent) = host.agent.clone() else {
                return Ok(());
            };
            if host.busy {
                return Ok(());
            }
            let Some(job) = store
                .queued_jobs()?
                .into_iter()
                .find(|j| j.topic == meta.topic)
            else {
                return Ok(());
            };
            let Some((msg, _)) = store.message(&job.op_hash)? else {
                store.set_job_state(&job.op_hash, JobState::Failed, Some("message not stored"))?;
                continue;
            };
            let asked_at_ms = msg.op_ts.map_or(msg.sent_at_ms, |t| t / 1000);
            if now_ms().saturating_sub(asked_at_ms) > agent.limits.max_age_ms {
                store.set_job_state(&job.op_hash, JobState::Refused, Some("too old"))?;
                continue;
            }
            let members = members(space).await;
            let input = turn_input(
                store,
                meta,
                self.identity.node_id(),
                &members,
                &agent,
                &job,
                &msg,
            )?;
            store.set_job_state(&job.op_hash, JobState::Started, None)?;
            host.busy = true;
            let done = host.done.clone();
            let allowed = ToolSet::parse(&job.tools);
            let op_hash = job.op_hash;
            tokio::spawn(async move {
                let result = triad_agent::run_turn(&agent, allowed, &NoAudit, &input)
                    .await
                    .map(|turn| turn.text)
                    .map_err(|e| format!("{e:#}"));
                let _ = done.send(JobDone { op_hash, result });
            });
            return Ok(());
        }
    }

    /// Publishes a finished turn's reply as the bot, in pieces that fit a
    /// space message (this demo sends no long messages), and marks the job.
    async fn finish(&self, done: JobDone, space: Option<&Space<RoomEvent>>, store: &mut State) {
        let published = async {
            let text = done.result.map_err(|e| anyhow!(e))?;
            let space = space.ok_or_else(|| anyhow!("room is not connected"))?;
            for piece in split_text(&text, SEND_TEXT_BYTES) {
                let body = ChatBody::new(&piece, now_ms())?.from_bot();
                space.publish(RoomEvent::Chat(body)).await?.await?;
            }
            anyhow::Ok(())
        }
        .await;
        let (state, reason) = match &published {
            Ok(()) => (JobState::Done, None),
            Err(e) => (JobState::Failed, Some(format!("{e:#}"))),
        };
        let _ = store.set_job_state(&done.op_hash, state, reason.as_deref());
    }
}

/// The room's members with their access, for the bot's policy.
async fn members(space: Option<&Space<RoomEvent>>) -> BTreeMap<NodeId, policy::Access> {
    let Some(space) = space else {
        return BTreeMap::new();
    };
    space
        .members()
        .await
        .map(|list| {
            list.iter()
                .map(|(id, a)| {
                    let access = match a {
                        AccessLevel::Manage => policy::Access::Manage,
                        _ => policy::Access::Member,
                    };
                    (*id.as_bytes(), access)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One turn's model input, from the room's own history only.
fn turn_input(
    store: &State,
    meta: &Meta,
    me: NodeId,
    members: &BTreeMap<NodeId, policy::Access>,
    agent: &Agent,
    job: &Job,
    msg: &StoredMessage,
) -> Result<context::TurnInput> {
    let no_grants = Vec::new();
    let no_blocks = BTreeSet::new();
    let view = ChannelView {
        host: me,
        bot_enabled: true,
        members,
        grants: &no_grants,
        blocked: &no_blocks,
        handled: false,
        counts: policy::Counts::default(),
    };
    let tier = |n: &NodeId| policy::tier(&view, n).as_str().to_string();
    let host = store.bot(&meta.topic)?.map(|b| b.host);
    let history: Vec<HistoryLine> = store
        .history(&meta.topic, Some(200))?
        .into_iter()
        .map(|m| HistoryLine {
            op_hash: m.op_hash,
            author: short_id(&m.author),
            tier: tier(&m.author),
            bot: m.bot && host == Some(m.author),
            text: m.text,
        })
        .collect();
    let member_list: Vec<(String, String)> =
        members.keys().map(|n| (short_id(n), tier(n))).collect();
    let allowed = ToolSet::parse(&job.tools);
    let tools = offered(&agent.all_tools, &allowed);
    let offered_servers = servers(&tools);
    let unavailable: Vec<String> = agent
        .servers()
        .into_iter()
        .filter(|s| !offered_servers.contains(s))
        .collect();
    let host = short_id(&me);
    let asker = short_id(&job.asker);
    let asker_tier = tier(&job.asker);
    Ok(context::build(
        Turn {
            kind: Kind::Channel,
            channel: &meta.name,
            persona: agent.persona.as_deref(),
            members: &member_list,
            history: &history,
            ask_hash: job.op_hash,
            asker: &asker,
            asker_tier: &asker_tier,
            ask: &msg.text,
            tools,
            unavailable,
            host: &host,
        },
        ContextLimits::default(),
    ))
}

struct Room {
    meta: Meta,
    events: SpaceSubscription<RoomEvent>,
    space: Option<Space<RoomEvent>>,
    joining: Option<PendingSpace<RoomEvent>>,
}

async fn open_room(
    node: &Node,
    store: &mut State,
    start: Start,
    me: NodeId,
    relay: String,
) -> Result<Room> {
    match start {
        Start::Create { .. } => {
            let (space, events) = node.create_space::<RoomEvent>().await?;
            let topic = *space.id().as_bytes();
            let meta = room_meta("general".into(), topic, me, relay, Role::Creator);
            store.insert_group(&meta)?;
            Ok(Room {
                meta,
                events,
                space: Some(space),
                joining: None,
            })
        }
        Start::Join(invite) => {
            let name = invite
                .name
                .as_deref()
                .and_then(|name| channel_name(name).ok())
                .unwrap_or_else(|| hex::encode(&invite.topic[..4]));
            let meta = room_meta(name, invite.topic, invite.creator, relay, Role::Member);
            let (joining, events) = node.space::<RoomEvent>(Hash::from(meta.topic)).await?;
            store.insert_group(&meta)?;
            Ok(Room {
                meta,
                events,
                space: None,
                joining: Some(joining),
            })
        }
        Start::Resume => {
            let meta = store.meta()?.ok_or_else(|| anyhow!("no saved room"))?;
            let (joining, events) = node.space::<RoomEvent>(Hash::from(meta.topic)).await?;
            Ok(Room {
                meta,
                events,
                space: None,
                joining: Some(joining),
            })
        }
    }
}

fn room_meta(name: String, topic: [u8; 32], creator: NodeId, relay: String, role: Role) -> Meta {
    Meta {
        name,
        topic,
        creator,
        relay,
        role,
        status: if role == Role::Creator {
            JoinStatus::Joined
        } else {
            JoinStatus::Pending
        },
    }
}

async fn publish(
    space: Option<&Space<RoomEvent>>,
    store: &mut State,
    topic: &[u8; 32],
    identity: &Identity,
    outgoing: Outgoing,
) -> Result<()> {
    let space = space.ok_or_else(|| anyhow!("room is not connected"))?;
    let body = match outgoing {
        Outgoing::Chat(body) => RoomEvent::Chat(body),
        Outgoing::Bot(_) => unreachable!("handled by the room loop"),
        Outgoing::Nickname(name) => {
            if store.profile(topic, &identity.node_id())?.as_deref() == Some(&name) {
                return Ok(());
            }
            let revision = store.next_profile_revision(topic)?;
            RoomEvent::Profile(ProfileUpdate::sign(identity, topic, revision, &name)?)
        }
    };
    space.publish(body).await?.await?;
    Ok(())
}

impl View {
    fn room(&mut self, meta: &Meta) -> Result<()> {
        self.group = hex::encode(meta.topic);
        self.creator = meta.role == Role::Creator;
        if meta.role == Role::Creator {
            self.invite = Invite::new(meta.topic, meta.creator, &meta.relay)
                .with_name(&meta.name)
                .render()?;
        }
        Ok(())
    }
}

impl Message {
    /// `host` is the room's accepted bot host: only its bot-flagged
    /// messages show as the bot's.
    fn from_stored(message: StoredMessage, me: NodeId, host: Option<NodeId>) -> Self {
        Self {
            author: hex::encode(message.author),
            nickname: message.nickname,
            bot: message.bot && host == Some(message.author),
            mine: message.author == me,
            text: message.text,
        }
    }
}
