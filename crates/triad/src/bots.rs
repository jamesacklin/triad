//! The bot side of a chat session: the personal bot's DM, channel bots, the
//! job queue and its worker, and the bot commands. The policy, context,
//! brain and tools live in `triad-agent`; this file connects them to the
//! session's spaces, state and display.
//!
//! A turn: a message is recorded with its job in one transaction
//! ([`Session::bot_job`]); one worker per channel takes queued jobs in order
//! ([`Session::pump`]), marks each started, and runs the brain on a spawned
//! task; the reply comes back to the event loop ([`Session::on_job_done`]),
//! which publishes it with `bot: true` and marks the job done. On start,
//! every started job is marked abandoned and never retried.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;
use triad_agent::context::{self, ContextLimits, HistoryLine, Kind, Turn};
use triad_agent::policy::{self, ChannelView, Decision, Incoming, Tier, ToolSet};
use triad_agent::tools::{Audit, offered, servers};
use triad_agent::{
    Agent, AgentConfig, KEYS_FILE, TurnResult, configured_model, save_key, save_model,
};
use triad_core::envelope::Control;
use triad_core::state::{BotRow, Job, JobState, NewJob};

use super::*;

/// The name of the personal bot's DM.
pub const DM_NAME: &str = "agent";

/// A line for the display, sent from a worker task to the event loop.
pub type BotEvent = (Value, String);

/// The running bots of a session (all but `--no-bot` sessions, once a
/// model is configured).
pub struct Bots {
    pub agent: Arc<Agent>,
    /// A second connection to `state.sqlite`, for the audit log, written
    /// from worker tasks.
    pub audit_db: Arc<Mutex<State>>,
    /// Channels whose worker is running a turn.
    pub busy: BTreeSet<Topic>,
}

/// A finished turn, back on the event loop.
pub struct JobDone {
    pub topic: Topic,
    pub op_hash: [u8; 32],
    pub asker: NodeId,
    pub result: Result<TurnResult, String>,
    /// When the asker's message was written (its operation timestamp).
    pub asked_at_ms: u64,
    pub started: Instant,
}

/// Writes the audit log for one job, from its worker task.
struct JobAudit {
    db: Arc<Mutex<State>>,
    /// Tool names by audit ID, for the `tool_call` events.
    names: Mutex<BTreeMap<i64, String>>,
    op_hash: [u8; 32],
    channel: String,
    asker: NodeId,
    events: mpsc::UnboundedSender<BotEvent>,
}

impl JobAudit {
    fn emit(&self, tool: &str, state: &str) {
        let value = json!({"event": "tool_call", "channel": self.channel,
                           "asker": hex_id(&self.asker), "tool": tool, "state": state,
                           "op": hex::encode(self.op_hash)});
        let text = format!(
            "[#{}] tool call by {}: {tool} {state}",
            self.channel,
            short_id(&self.asker)
        );
        let _ = self.events.send((value, text));
    }

    fn db(&self) -> std::sync::MutexGuard<'_, State> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Audit for JobAudit {
    fn sent(&self, tool: &str, args: &Value) -> i64 {
        let id = self
            .db()
            .tool_call_sent(&self.op_hash, tool, &args.to_string())
            .unwrap_or_else(|e| {
                tracing::error!("audit log: {e:#}");
                -1
            });
        self.names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tool.to_string());
        self.emit(tool, "sent");
        id
    }

    fn finished(&self, id: i64, ok: bool, result_bytes: usize) {
        if let Err(e) = self.db().tool_call_finished(id, ok, result_bytes as u64) {
            tracing::error!("audit log: {e:#}");
        }
        let tool = self
            .names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
            .unwrap_or_default();
        self.emit(&tool, if ok { "ok" } else { "error" });
    }

    fn denied(&self, tool: &str, args: &Value, why: &str) {
        if let Err(e) = self
            .db()
            .tool_call_denied(&self.op_hash, tool, &args.to_string())
        {
            tracing::error!("audit log: {e:#}");
        }
        tracing::info!(tool, why, "tool call denied");
        self.emit(tool, "denied");
    }
}

fn access_level(a: AccessLevel) -> policy::Access {
    match a {
        AccessLevel::Manage => policy::Access::Manage,
        _ => policy::Access::Member,
    }
}

impl Session {
    pub fn is_dm(&self, topic: &Topic) -> bool {
        self.dm == Some(*topic)
    }

    /// Reports that this session runs without bots, and why.
    pub fn bots_unavailable(&self, why: &str) {
        self.printer
            .emit(json!({"event": "bot_unavailable", "reason": why}), || {
                format!("bots off: {why}. /bot on retries; --no-bot silences this.")
            });
    }

    /// Starts the bots: the brain, tools, the DM, and any jobs left queued.
    pub async fn start_bots(&mut self, config: &AgentConfig, streams: &mut Streams) -> Result<()> {
        let data_dir = self.data_dir.clone();
        let data_dir = data_dir.as_path();
        let agent = Agent::start(config, data_dir).await?;
        for w in &agent.warnings {
            self.printer.info(w);
        }
        for job in self.state.abandon_started()? {
            self.emit_job(
                &job.topic,
                &job.op_hash,
                &job.asker,
                JobState::Abandoned,
                Some("interrupted"),
                None,
            );
        }
        self.bots = Some(Bots {
            agent: Arc::new(agent),
            audit_db: Arc::new(Mutex::new(State::open(data_dir)?)),
            busy: BTreeSet::new(),
        });
        self.ensure_dm(streams).await?;
        Ok(())
    }

    /// Runs jobs left queued by an earlier session.
    pub async fn resume_jobs(&mut self, streams: &mut Streams) {
        let topics: BTreeSet<Topic> = match self.state.queued_jobs() {
            Ok(jobs) => jobs.iter().map(|j| j.topic).collect(),
            Err(e) => {
                self.printer.error(&format!("reading bot jobs: {e:#}"));
                return;
            }
        };
        for topic in topics {
            self.pump(topic, streams).await;
        }
    }

    /// The personal bot's DM: a space named `agent` with this node as its
    /// only member, created the first time chat mode starts with bots.
    async fn ensure_dm(&mut self, streams: &mut Streams) -> Result<Topic> {
        if let Some(t) = self.dm
            && self.channels.contains_key(&t)
        {
            return Ok(t);
        }
        if let Some(c) = self.channels.values().find(|c| c.meta.name == DM_NAME) {
            bail!(
                "a channel named #{} already exists, and the bot DM needs that name",
                c.meta.name
            );
        }
        let (space, rx) = self.node.create_space::<RoomEvent>().await?;
        let topic = *space.id().as_bytes();
        let meta = Meta {
            name: DM_NAME.into(),
            topic,
            creator: self.me,
            relay: self.home_relay.clone(),
            role: Role::Creator,
            status: JoinStatus::Joined,
        };
        self.state.insert_group(&meta)?;
        self.state.set_dm_topic(&topic)?;
        self.dm = Some(topic);
        tracing::info!(space = %hex::encode(&topic[..4]), "created bot DM");
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
        Ok(topic)
    }

    /// Whether `@bot` can address a bot in the channel.
    pub fn has_bot(&self, topic: &Topic) -> bool {
        self.is_dm(topic) || matches!(self.state.bot(topic), Ok(Some(b)) if b.enabled)
    }

    /// Whether a message claiming to come from a bot is shown as the
    /// channel's bot: only when its author is the channel's accepted host
    /// (in the DM, this node).
    pub fn bot_accepted(&self, topic: &Topic, author: &NodeId) -> bool {
        let host = match self.is_dm(topic) {
            true => None,
            false => self.state.bot(topic).ok().flatten().map(|b| b.host),
        };
        bot_author_accepted(self.is_dm(topic), &self.me, host.as_ref(), author)
    }

    /// The host's view of a channel's members, for the policy.
    fn access_map(&self, topic: &Topic) -> BTreeMap<NodeId, policy::Access> {
        self.channels
            .get(topic)
            .map(|c| {
                c.members
                    .iter()
                    .map(|(id, a)| (*id, access_level(*a)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether this node runs the channel's bot right now.
    fn hosting(&self, topic: &Topic) -> bool {
        self.bots.is_some()
            && matches!(self.state.bot(topic), Ok(Some(b)) if b.enabled && b.host == self.me)
    }

    /// Asks the policy about a message about to be recorded. Returns the job
    /// to insert with it, if any.
    pub async fn bot_job(
        &mut self,
        topic: Topic,
        op_hash: [u8; 32],
        msg: &Incoming,
    ) -> Result<Option<NewJob>> {
        let Some(bots) = &self.bots else {
            return Ok(None);
        };
        let limits = bots.agent.limits;
        let now = now_ms();
        let handled = self.state.job(&op_hash)?.is_some();
        let decision = if self.is_dm(&topic) {
            policy::decide_dm(msg, &self.me, handled, &limits, now)
        } else {
            if !msg.ask || msg.bot || msg.ctl || !self.hosting(&topic) {
                return Ok(None);
            }
            // The host's view of the member list at the moment the message
            // is processed.
            self.refresh_members(topic).await;
            let members = self.access_map(&topic);
            let grants = self.state.grants(&topic)?;
            let blocked = self.state.blocked()?;
            let c = self.state.answer_counts(&topic, &msg.author, now)?;
            let view = ChannelView {
                host: self.me,
                bot_enabled: true,
                members: &members,
                grants: &grants,
                blocked: &blocked,
                handled,
                counts: policy::Counts {
                    asker_last_hour: c.asker_last_hour,
                    channel_last_day: c.channel_last_day,
                    channel_last_minute: c.channel_last_minute,
                },
            };
            policy::decide_channel(msg, &view, &limits, now)
        };
        Ok(match decision {
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
        })
    }

    pub fn emit_job(
        &self,
        topic: &Topic,
        op_hash: &[u8; 32],
        asker: &NodeId,
        state: JobState,
        reason: Option<&str>,
        extra: Option<Value>,
    ) {
        let name = self.name(topic).to_string();
        let mut value = json!({"event": "job", "channel": name, "op": hex::encode(op_hash),
                               "asker": hex_id(asker), "state": state.as_str(), "reason": reason});
        if let (Some(Value::Object(extra)), Some(fields)) = (extra, value.as_object_mut()) {
            fields.extend(extra);
        }
        let text = match reason {
            Some(r) => format!(
                "[#{name}] bot job for {}: {} ({r})",
                short_id(asker),
                state.as_str()
            ),
            None => format!(
                "[#{name}] bot job for {}: {}",
                short_id(asker),
                state.as_str()
            ),
        };
        self.printer.emit(value, || text);
    }

    /// Starts the channel's next queued job, unless its worker is busy.
    pub async fn pump(&mut self, topic: Topic, streams: &mut Streams) {
        loop {
            let Some(bots) = &self.bots else { return };
            if bots.busy.contains(&topic) {
                return;
            }
            let job = match self.state.queued_jobs() {
                Ok(jobs) => jobs.into_iter().find(|j| j.topic == topic),
                Err(e) => {
                    self.printer.error(&format!("reading bot jobs: {e:#}"));
                    return;
                }
            };
            let Some(job) = job else { return };
            match self.start_job(&job, streams) {
                Ok(true) => return,
                Ok(false) => continue,
                Err(e) => {
                    let reason = format!("{e:#}");
                    let _ = self
                        .state
                        .set_job_state(&job.op_hash, JobState::Failed, Some(&reason));
                    self.emit_job(
                        &topic,
                        &job.op_hash,
                        &job.asker,
                        JobState::Failed,
                        Some(&reason),
                        None,
                    );
                }
            }
        }
    }

    /// Marks a job started and spawns its turn. Returns false when the job
    /// was refused instead (its message is too old by now).
    fn start_job(&mut self, job: &Job, streams: &mut Streams) -> Result<bool> {
        let (msg, _) = self
            .state
            .message(&job.op_hash)?
            .context("the job's message is not stored")?;
        let bots = self.bots.as_ref().expect("checked by pump");
        let asked_at_ms = msg.op_ts.map_or(msg.sent_at_ms, |t| t / 1000);
        if now_ms().saturating_sub(asked_at_ms) > bots.agent.limits.max_age_ms {
            self.state
                .set_job_state(&job.op_hash, JobState::Refused, Some("too old"))?;
            self.emit_job(
                &job.topic,
                &job.op_hash,
                &job.asker,
                JobState::Refused,
                Some("too old"),
                None,
            );
            return Ok(false);
        }
        let input = self.turn_input(job, &msg)?;
        self.state
            .set_job_state(&job.op_hash, JobState::Started, None)?;
        self.emit_job(
            &job.topic,
            &job.op_hash,
            &job.asker,
            JobState::Started,
            None,
            None,
        );
        let bots = self.bots.as_mut().expect("checked by pump");
        bots.busy.insert(job.topic);
        let agent = bots.agent.clone();
        let audit = JobAudit {
            db: bots.audit_db.clone(),
            names: Mutex::new(BTreeMap::new()),
            op_hash: job.op_hash,
            channel: self.name(&job.topic).to_string(),
            asker: job.asker,
            events: self.bot_events.clone(),
        };
        let allowed = ToolSet::parse(&job.tools);
        let (topic, op_hash, asker) = (job.topic, job.op_hash, job.asker);
        let started = Instant::now();
        streams.jobs.spawn(async move {
            let result = triad_agent::run_turn(&agent, allowed, &audit, &input)
                .await
                .map_err(|e| format!("{e:#}"));
            JobDone {
                topic,
                op_hash,
                asker,
                result,
                asked_at_ms,
                started,
            }
        });
        Ok(true)
    }

    /// Builds one turn's model input from the job's own channel only.
    fn turn_input(&self, job: &Job, msg: &StoredMessage) -> Result<context::TurnInput> {
        let bots = self.bots.as_ref().expect("bots are running");
        let channel = self.channel(&job.topic)?;
        let dm = self.is_dm(&job.topic);
        let members = self.access_map(&job.topic);
        let no_grants = Vec::new();
        let no_blocks = BTreeSet::new();
        let view = ChannelView {
            host: self.me,
            bot_enabled: true,
            members: &members,
            grants: &no_grants,
            blocked: &no_blocks,
            handled: false,
            counts: policy::Counts::default(),
        };
        let tier = |n: &NodeId| policy::tier(&view, n).as_str().to_string();
        let history: Vec<HistoryLine> = self
            .state
            .history(&job.topic, Some(200))?
            .into_iter()
            .map(|m| HistoryLine {
                op_hash: m.op_hash,
                author: short_id(&m.author),
                tier: tier(&m.author),
                bot: m.bot && self.bot_accepted(&job.topic, &m.author),
                // The model reads an action as IRC writes it.
                text: if m.action {
                    format!("/me {}", m.text)
                } else {
                    m.text
                },
            })
            .collect();
        let member_list: Vec<(String, String)> =
            members.keys().map(|n| (short_id(n), tier(n))).collect();
        let allowed = ToolSet::parse(&job.tools);
        let tools = offered(&bots.agent.all_tools, &allowed);
        let offered_servers = servers(&tools);
        let unavailable: Vec<String> = bots
            .agent
            .servers()
            .into_iter()
            .filter(|s| !offered_servers.contains(s))
            .collect();
        let asker_tier = if dm {
            Tier::Host.as_str().to_string()
        } else {
            tier(&job.asker)
        };
        let host = short_id(&self.me);
        let asker = short_id(&job.asker);
        Ok(context::build(
            Turn {
                kind: if dm { Kind::Dm } else { Kind::Channel },
                channel: &channel.meta.name,
                persona: bots.agent.persona.as_deref(),
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

    /// A turn finished: publish the reply with `bot: true`, then mark the
    /// job done. A crash between the two leaves the job started, so it is
    /// abandoned on restart and the reply is never sent twice.
    pub async fn on_job_done(&mut self, done: JobDone, streams: &mut Streams) {
        if let Some(bots) = &mut self.bots {
            bots.busy.remove(&done.topic);
        }
        let JobDone {
            topic,
            op_hash,
            asker,
            result,
            asked_at_ms,
            started,
        } = done;
        match result {
            Ok(turn) => {
                let published = async {
                    let body = ChatBody::new(&turn.text, now_ms())?.from_bot();
                    self.publish_chat(topic, body).await?;
                    anyhow::Ok(())
                }
                .await;
                match published {
                    Ok(()) => {
                        let _ = self.state.set_job_state(&op_hash, JobState::Done, None);
                        let extra = json!({
                            "turn_ms": started.elapsed().as_millis() as u64,
                            "model_ms": turn.output.model_time.as_millis() as u64,
                            "tool_ms": turn.tool_time.as_millis() as u64,
                            "requests": turn.output.requests,
                            "input_tokens": turn.output.input_tokens,
                            "output_tokens": turn.output.output_tokens,
                            "since_ask_ms": now_ms().saturating_sub(asked_at_ms),
                            "denied": turn.denied,
                        });
                        self.emit_job(&topic, &op_hash, &asker, JobState::Done, None, Some(extra));
                    }
                    Err(e) => {
                        let reason = format!("publishing the reply: {e:#}");
                        let _ = self
                            .state
                            .set_job_state(&op_hash, JobState::Failed, Some(&reason));
                        self.emit_job(
                            &topic,
                            &op_hash,
                            &asker,
                            JobState::Failed,
                            Some(&reason),
                            None,
                        );
                    }
                }
            }
            Err(e) => {
                let _ = self
                    .state
                    .set_job_state(&op_hash, JobState::Failed, Some(&e));
                self.emit_job(&topic, &op_hash, &asker, JobState::Failed, Some(&e), None);
            }
        }
        self.pump(topic, streams).await;
    }

    /// Handles an announcement. A client accepts the first `BotOn` it sees
    /// in a channel from a member with `Manage` access, and from then on
    /// accepts `BotOn` and `BotOff` only from that same author.
    pub async fn on_control(&mut self, topic: Topic, author: NodeId, ctl: &Control) -> Result<()> {
        if self.is_dm(&topic) {
            return Ok(());
        }
        let name = self.name(&topic).to_string();
        let current = self.state.bot(&topic)?;
        let accepted = match &current {
            Some(b) => b.host == author,
            None => {
                self.refresh_members(topic).await;
                matches!(ctl, Control::BotOn { .. })
                    && self
                        .channel(&topic)?
                        .members
                        .get(&author)
                        .is_some_and(|a| *a == AccessLevel::Manage)
            }
        };
        if !accepted {
            self.printer.emit(
                json!({"event": "bot_rejected", "channel": name, "author": hex_id(&author),
                       "host": current.as_ref().map(|b| hex_id(&b.host))}),
                || {
                    format!(
                        "[#{name}] bot announcement from {} not accepted",
                        short_id(&author)
                    )
                },
            );
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
        self.state.set_bot(&BotRow {
            topic,
            host: author,
            provider: provider.clone(),
            servers: servers.clone(),
            enabled,
            changed_at_ms: now_ms(),
        })?;
        let event = if enabled { "bot_on" } else { "bot_off" };
        self.printer.emit(
            json!({"event": event, "channel": name, "host": hex_id(&author),
                   "provider": provider, "servers": servers}),
            || {
                let who = if author == self.me {
                    "me".to_string()
                } else {
                    short_id(&author)
                };
                if enabled {
                    format!(
                        "[#{name}] bot on, run by {who}: sends to {provider}; servers: {}",
                        if servers.is_empty() {
                            "none".to_string()
                        } else {
                            servers.join(", ")
                        }
                    )
                } else {
                    format!("[#{name}] bot off ({who})")
                }
            },
        );
        Ok(())
    }

    /// The readable announcement members see, and the control itself.
    fn bot_on_body(&self) -> Result<ChatBody> {
        let bots = self.bots.as_ref().context("this session has no bots")?;
        let provider = bots.agent.provider();
        let servers = bots.agent.servers();
        let text = format!(
            "This channel has a bot, run by me. It sends the channel's messages to {provider} and can use: {}.",
            if servers.is_empty() {
                "no tools".to_string()
            } else {
                servers.join(", ")
            }
        );
        Ok(ChatBody::new(&text, now_ms())?.with_ctl(Control::BotOn { provider, servers }))
    }

    /// Adds my bot to a channel this node created, and announces it. In a
    /// session running without bots (`--no-bot`, or no model at start),
    /// starts them first, so a bot can join a channel at any time.
    pub async fn bot_on(&mut self, topic: Topic, streams: &mut Streams) -> Result<()> {
        let c = self.channel(&topic)?;
        if self.is_dm(&topic) {
            bail!("the bot DM always has your bot");
        }
        if c.meta.role != Role::Creator {
            bail!("only the channel's creator can run its bot");
        }
        if c.meta.status != JoinStatus::Joined {
            bail!("not a member of #{} yet", c.meta.name);
        }
        self.ensure_bots(streams).await?;
        let body = self.bot_on_body()?;
        self.publish_chat(topic, body).await?;
        Ok(())
    }

    /// `/bot model`: shows the model, or saves a new one and applies it.
    async fn bot_model(&mut self, model: &str, streams: &mut Streams) -> Result<()> {
        if model.is_empty() {
            let current = configured_model(&self.data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
            let running = self.bots.as_ref().map(|b| b.agent.provider());
            self.printer.emit(
                json!({"event": "bot_model", "model": current, "running": running}),
                || match &running {
                    Some(r) if *r != current => {
                        format!("bot model: {current} (running: {r})")
                    }
                    _ => format!("bot model: {current}"),
                },
            );
            return Ok(());
        }
        if model.split_whitespace().count() != 1 {
            bail!("usage: /bot model <vendor/model>");
        }
        save_model(&self.data_dir, model)?;
        self.printer.info(&format!("bot model set to {model}"));
        if std::env::var("TRIAD_AGENT_MODEL").is_ok_and(|v| !v.is_empty()) {
            self.printer
                .info("TRIAD_AGENT_MODEL is set and wins over /bot model");
        }
        self.apply_bot_config(streams).await
    }

    /// `/bot provider-key [vendor] <key>`: saves a key for the configured
    /// model's vendor (or the one named) and applies it. The key is not
    /// echoed or logged.
    async fn bot_provider_key(&mut self, arg: &str, streams: &mut Streams) -> Result<()> {
        let usage = "usage: /bot provider-key [vendor] <key>";
        let (vendor, key) = match arg.split_whitespace().collect::<Vec<_>>()[..] {
            [key] => {
                let model = configured_model(&self.data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
                let vendor = triad_agent::brain::vendor_of(&model)
                    .map_err(anyhow::Error::msg)?
                    .to_string();
                (vendor, key)
            }
            [vendor, key] => (vendor.to_string(), key),
            _ => bail!(usage),
        };
        save_key(&self.data_dir, &vendor, key)?;
        self.printer.info(&format!(
            "saved the {vendor} key in {} (readable only by you)",
            self.data_dir.join(KEYS_FILE).display()
        ));
        if let Some(var) = triad_agent::brain::key_var_set(&vendor) {
            self.printer
                .info(&format!("{var} is set and wins over the saved key"));
        }
        self.apply_bot_config(streams).await
    }

    /// After the model or a key changed: starts the bots if they are not
    /// running, or gives the running ones the new brain and tells each
    /// channel whose bot I run where its messages now go.
    async fn apply_bot_config(&mut self, streams: &mut Streams) -> Result<()> {
        let config = match AgentConfig::load(&self.data_dir) {
            Ok(c) => c,
            Err(e) => {
                self.printer.info(&format!("bots not started: {e}"));
                return Ok(());
            }
        };
        let Some(bots) = self.bots.as_mut() else {
            return self.ensure_bots(streams).await;
        };
        let before = bots.agent.provider();
        bots.agent = Arc::new(bots.agent.with_brain(&config)?);
        let provider = bots.agent.provider();
        self.printer.emit(
            json!({"event": "bots_started", "provider": provider, "servers": bots.agent.servers()}),
            || format!("bots now use {provider}"),
        );
        if provider != before {
            let hosted: Vec<Topic> = self
                .channels
                .keys()
                .copied()
                .filter(|t| !self.is_dm(t) && self.hosting(t))
                .collect();
            for topic in hosted {
                let body = self.bot_on_body()?;
                self.publish_chat(topic, body).await?;
            }
        }
        Ok(())
    }

    /// Starts the bots in a session running without them.
    async fn ensure_bots(&mut self, streams: &mut Streams) -> Result<()> {
        if self.bots.is_some() {
            return Ok(());
        }
        let config = AgentConfig::load(&self.data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
        let had_dm = self.dm.is_some_and(|t| self.channels.contains_key(&t));
        self.start_bots(&config, streams).await?;
        let bots = self.bots.as_ref().expect("just started");
        let (provider, servers) = (bots.agent.provider(), bots.agent.servers());
        self.printer.emit(
            json!({"event": "bots_started", "provider": provider, "servers": servers}),
            || format!("bots started: {provider}"),
        );
        if !had_dm && let Some(dm) = self.dm {
            // Report the new DM without moving the focus off the channel.
            let c = self.channel(&dm)?;
            let mut value = self.channel_json(c)?;
            value["event"] = json!("channel");
            self.printer.emit(value, || {
                format!("your bot's DM is #{DM_NAME}; /switch {DM_NAME} to talk to it")
            });
        }
        self.resume_jobs(streams).await;
        Ok(())
    }

    pub async fn bot_off(&mut self, topic: Topic) -> Result<()> {
        let c = self.channel(&topic)?;
        if self.is_dm(&topic) {
            bail!("the bot DM always has your bot");
        }
        if c.meta.role != Role::Creator {
            bail!("only the channel's creator can run its bot");
        }
        let body = ChatBody::new("This channel's bot is off.", now_ms())?.with_ctl(Control::BotOff);
        self.publish_chat(topic, body).await?;
        Ok(())
    }

    /// A member by full hex ID or unique prefix, in the focused channel.
    fn resolve_member(&self, topic: &Topic, text: &str) -> Result<NodeId> {
        let text = text.trim().to_ascii_lowercase();
        if text.is_empty() {
            bail!("missing node ID");
        }
        if text.len() == 64 {
            return parse_node(&text);
        }
        let c = self.channel(topic)?;
        let matches: Vec<&NodeId> = c
            .members
            .keys()
            .filter(|m| hex_id(m).starts_with(&text))
            .collect();
        match matches.as_slice() {
            [one] => Ok(**one),
            [] => bail!("no member of #{} starts with {text}", c.meta.name),
            _ => bail!("{text} matches more than one member; type more of it"),
        }
    }

    fn require_host(&self, topic: &Topic) -> Result<()> {
        if self.is_dm(topic) {
            bail!("the bot DM has no other members");
        }
        match self.state.bot(topic)? {
            Some(b) if b.host == self.me => Ok(()),
            _ => bail!("only the host of #{}'s bot can do that", self.name(topic)),
        }
    }

    /// The bot commands. Returns false if `cmd` is not one of them.
    pub async fn on_bot_command(
        &mut self,
        cmd: &str,
        rest: &str,
        streams: &mut Streams,
    ) -> Result<bool> {
        let focus = self.focus;
        let name = self.name(&focus).to_string();
        match cmd {
            "/bot" => match rest.split_once(char::is_whitespace).unwrap_or((rest, "")) {
                ("on", "") => self.bot_on(focus, streams).await?,
                ("off", "") => self.bot_off(focus).await?,
                ("model", arg) => self.bot_model(arg.trim(), streams).await?,
                ("provider-key", arg) => self.bot_provider_key(arg.trim(), streams).await?,
                ("", _) => {
                    let bot = self.state.bot(&focus)?;
                    let mine = match &bot {
                        _ if self.is_dm(&focus) => "all".to_string(),
                        Some(b) if b.host == self.me => "all".to_string(),
                        Some(_) => {
                            self.refresh_members(focus).await;
                            let manage = self.channel(&focus)?.members.get(&self.me)
                                == Some(&AccessLevel::Manage);
                            if manage {
                                "all".into()
                            } else {
                                "none, unless the host granted some".into()
                            }
                        }
                        None => "none".to_string(),
                    };
                    let (host, provider, servers, enabled) = match &bot {
                        _ if self.is_dm(&focus) => (
                            Some(self.me),
                            self.bots.as_ref().map(|b| b.agent.provider()),
                            self.bots
                                .as_ref()
                                .map(|b| b.agent.servers())
                                .unwrap_or_default(),
                            self.bots.is_some(),
                        ),
                        Some(b) => (
                            Some(b.host),
                            Some(b.provider.clone()),
                            b.servers.clone(),
                            b.enabled,
                        ),
                        None => (None, None, Vec::new(), false),
                    };
                    self.printer.emit(
                        json!({"event": "bot", "channel": name, "host": host.as_ref().map(hex_id),
                               "provider": provider, "servers": servers, "enabled": enabled,
                               "my_tools": mine}),
                        || match host {
                            Some(h) if enabled => format!(
                                "#{name} bot: run by {}, sends to {}, servers: {}; your tools: {mine}",
                                if h == self.me { "me".into() } else { short_id(&h) },
                                provider.clone().unwrap_or_default(),
                                if servers.is_empty() { "none".into() } else { servers.join(", ") }
                            ),
                            _ => format!("#{name} has no bot"),
                        },
                    );
                }
                _ => bail!("usage: /bot [on|off|model [vendor/model]|provider-key [vendor] <key>]"),
            },
            "/grant" => {
                self.require_host(&focus)?;
                let (node, pattern) = rest
                    .split_once(char::is_whitespace)
                    .context("usage: /grant <node> <server.tool | server.*>")?;
                let pattern = pattern.trim();
                if !policy::pattern_valid(pattern) {
                    bail!("a grant is server.tool or server.*, got {pattern:?}");
                }
                let node = self.resolve_member(&focus, node)?;
                self.state.grant(&focus, &node, pattern)?;
                self.printer.emit(
                    json!({"event": "granted", "channel": name, "member": hex_id(&node), "pattern": pattern}),
                    || format!("granted {pattern} to {} in #{name}", short_id(&node)),
                );
            }
            "/revoke" => {
                self.require_host(&focus)?;
                let mut words = rest.split_whitespace();
                let node = words.next().context("usage: /revoke <node> [pattern]")?;
                let node = self.resolve_member(&focus, node)?;
                let pattern = words.next();
                let n = self.state.revoke(&focus, &node, pattern)?;
                self.printer.emit(
                    json!({"event": "revoked", "channel": name, "member": hex_id(&node),
                           "pattern": pattern, "removed": n}),
                    || format!("revoked {n} grant(s) from {} in #{name}", short_id(&node)),
                );
            }
            "/grants" => {
                self.require_host(&focus)?;
                let grants = self.state.grants(&focus)?;
                let list: Vec<Value> = grants
                    .iter()
                    .map(|(n, p)| json!({"member": hex_id(n), "pattern": p}))
                    .collect();
                self.printer.emit(
                    json!({"event": "grants", "channel": name, "grants": list}),
                    || {
                        if grants.is_empty() {
                            format!("no grants in #{name}")
                        } else {
                            grants
                                .iter()
                                .map(|(n, p)| format!("{}  {p}", short_id(n)))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    },
                );
            }
            "/block" | "/unblock" => {
                let node = if rest.len() == 64 {
                    parse_node(rest)?
                } else {
                    self.resolve_member(&focus, rest)?
                };
                if node == self.me {
                    bail!("you cannot block yourself");
                }
                let blocked = cmd == "/block";
                if blocked {
                    self.state.block(&node)?;
                } else {
                    self.state.unblock(&node)?;
                }
                self.printer.emit(
                    json!({"event": if blocked { "blocked" } else { "unblocked" }, "member": hex_id(&node)}),
                    || format!("{} {}", if blocked { "blocked" } else { "unblocked" }, short_id(&node)),
                );
            }
            "/audit" => {
                self.require_host(&focus)?;
                let n = if rest.is_empty() {
                    20
                } else {
                    rest.parse().context("usage: /audit [N]")?
                };
                let calls = self.state.tool_calls(&focus, n)?;
                let now = now_ms();
                let list: Vec<Value> = calls
                    .iter()
                    .map(|c| {
                        json!({"asker": hex_id(&c.asker), "tool": c.tool, "state": c.state,
                                    "result_bytes": c.result_bytes, "at_ms": c.at_ms,
                                    "op": hex::encode(c.op_hash)})
                    })
                    .collect();
                self.printer.emit(
                    json!({"event": "audit", "channel": name, "calls": list}),
                    || {
                        if calls.is_empty() {
                            return format!("no tool calls in #{name}");
                        }
                        calls
                            .iter()
                            .map(|c| {
                                format!(
                                    "{}  {}  {}  {} ago",
                                    short_id(&c.asker),
                                    c.tool,
                                    c.state,
                                    ago(now.saturating_sub(c.at_ms))
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                );
            }
            "/debug-bot-on" if std::env::var_os("TRIAD_AGENT_TEST_HOOKS").is_some() => {
                // Test hook: publish a BotOn as if this node ran the bot,
                // whatever its role, to check that members reject it.
                let body =
                    ChatBody::new("forged bot announcement", now_ms())?.with_ctl(Control::BotOn {
                        provider: "forged".into(),
                        servers: vec![],
                    });
                self.publish_chat(focus, body).await?;
            }
            "/debug-bot-says" if std::env::var_os("TRIAD_AGENT_TEST_HOOKS").is_some() => {
                // Test hook: a message with the bot flag from a non-host.
                let body = ChatBody::new(rest, now_ms())?.from_bot();
                self.publish_chat(focus, body).await?;
            }
            "/debug-long-hold" if std::env::var_os("TRIAD_AGENT_TEST_HOOKS").is_some() => {
                // Test hook: publish a long message's space message and keep
                // its ciphertext back until `/debug-long-release`, so readers
                // see the key before the body.
                let (long, payload) = triad_core::long::seal(rest)?;
                let body = ChatBody::new(rest, now_ms())?.into_long(long);
                self.joined_space(&focus)?
                    .publish(RoomEvent::Chat(body))
                    .await?;
                self.held_long = Some((focus, payload));
            }
            "/debug-long-release" if std::env::var_os("TRIAD_AGENT_TEST_HOOKS").is_some() => {
                let (topic, payload) = self.held_long.take().context("nothing held")?;
                self.long_pubs
                    .get(&topic)
                    .context("no long-message stream")?
                    .publish(payload)
                    .await?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

fn ago(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}
