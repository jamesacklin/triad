//! Chat mode: stdin lines in, events out. Every message is MLS-encrypted
//! before p2panda sees it; `group.rs` decides what each operation means.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use triad_core::envelope::{ChatBody, Envelope, Invite};
use triad_core::group::{self, Effect, GroupView, OpInput};
use triad_core::identity::{Identity, NodeId, hex_id, short_id};
use triad_core::mls::Mls;
use triad_core::net::{Incoming, Net, NetConfig, NetEvent, NetEvents, random_topic};
use triad_core::state::{JoinStatus, Meta, Outcome, Role, State, StoredMessage, now_ms};

use crate::output::Printer;

/// How long a joiner waits for a Welcome before printing a hint.
const WELCOME_HINT_AFTER: Duration = Duration::from_secs(120);

#[derive(Clone, Copy)]
pub struct Options {
    pub auto_accept: bool,
    pub json: bool,
    pub verbose: bool,
}

/// Epoch and member list from `mls.sqlite`, for `triad status`.
pub fn mls_summary(
    data_dir: &Path,
    identity: &Identity,
    meta: &Meta,
) -> Result<Option<(u64, Vec<NodeId>)>> {
    let mut mls = Mls::open_dir(data_dir, identity.node_id())?;
    if !mls.load_group(&meta.group_id)? {
        return Ok(None);
    }
    Ok(Some((mls.epoch()?, mls.members()?)))
}

pub async fn create(data_dir: &Path, relay: &str, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    if state.meta()?.is_some() {
        bail!("this data directory already holds a group; use `triad chat` or a fresh --data-dir");
    }
    let topic = random_topic();
    let mut mls = Mls::open_dir(data_dir, identity.node_id())?;
    mls.create_group(&topic)?;
    let meta = Meta {
        topic,
        group_id: topic.to_vec(),
        creator: identity.node_id(),
        relay: relay.to_string(),
        role: Role::Creator,
        status: JoinStatus::Joined,
        key_package_published: false,
        joined_epoch: Some(0),
    };
    state.insert_meta(&meta)?;
    tracing::info!(group = %hex::encode(&topic[..4]), "created group");
    run(data_dir, identity, state, mls, opts).await
}

pub async fn join(data_dir: &Path, invite: Invite, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    if invite.creator == identity.node_id() {
        bail!("this invite was created by this data directory");
    }
    let mls = Mls::open_dir(data_dir, identity.node_id())?;
    let meta = Meta {
        topic: invite.topic,
        group_id: invite.topic.to_vec(),
        creator: invite.creator,
        relay: invite.relay.clone(),
        role: Role::Member,
        status: JoinStatus::Pending,
        key_package_published: false,
        joined_epoch: None,
    };
    state.insert_meta(&meta)?;
    run(data_dir, identity, state, mls, opts).await
}

pub async fn resume(data_dir: &Path, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    let meta = state
        .meta()?
        .context("no group in this data directory; run `triad create` or `triad join`")?;
    let mut mls = Mls::open_dir(data_dir, identity.node_id())?;
    let loaded = mls.load_group(&meta.group_id)?;
    if meta.status == JoinStatus::Joined && !loaded {
        bail!("state.sqlite says joined but mls.sqlite holds no group");
    }
    run(data_dir, identity, state, mls, opts).await
}

/// A join request waiting for the user's answer. Its operation stays unacked.
struct PendingJoin {
    op: Incoming,
    member: NodeId,
    key_package: Vec<u8>,
}

struct Session {
    me: NodeId,
    state: State,
    meta: Meta,
    mls: Mls,
    net: Net,
    printer: Printer,
    opts: Options,
    asks: VecDeque<PendingJoin>,
    deferred: VecDeque<Incoming>,
    started: Instant,
    hinted: bool,
}

async fn run(
    data_dir: &Path,
    identity: Identity,
    state: State,
    mls: Mls,
    opts: Options,
) -> Result<()> {
    let meta = state.meta()?.context("missing meta")?;
    let me = identity.node_id();
    let bootstrap = (meta.role == Role::Member).then_some(meta.creator);
    let (net, mut events) = Net::start(
        &identity,
        NetConfig {
            data_dir,
            relay: &meta.relay,
            bootstrap,
            topic: meta.topic,
        },
    )
    .await?;
    let printer = Printer {
        json: opts.json,
        verbose: opts.verbose,
        me,
    };
    let mut session = Session {
        me,
        state,
        meta,
        mls,
        net,
        printer,
        opts,
        asks: VecDeque::new(),
        deferred: VecDeque::new(),
        started: Instant::now(),
        hinted: false,
    };
    let result = session.start(&mut events).await;
    session.net.shutdown().await;
    result
}

/// Acknowledges an operation. `TRIAD_TEST_SKIP_ACK=1` withholds every ack, so
/// tests can check what p2panda replays after a crash.
async fn ack(op: &Incoming) -> Result<()> {
    if std::env::var_os("TRIAD_TEST_SKIP_ACK").is_some() {
        tracing::debug!(op = %hex::encode(op.hash), "ack withheld (TRIAD_TEST_SKIP_ACK)");
        return Ok(());
    }
    op.ack().await
}

fn spawn_stdin() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let l = line.trim_end_matches(['\n', '\r']).to_string();
                    if tx.blocking_send(l).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

impl Session {
    fn view(&self) -> GroupView {
        GroupView {
            me: self.me,
            role: self.meta.role,
            status: self.meta.status,
            creator: self.meta.creator,
            group_id: self.meta.group_id.clone(),
            joined_epoch: self.meta.joined_epoch,
        }
    }

    fn epoch_and_members(&self) -> (Option<u64>, usize) {
        if !self.mls.has_group() {
            return (None, 0);
        }
        (
            self.mls.epoch().ok(),
            self.mls.members().map(|m| m.len()).unwrap_or(0),
        )
    }

    async fn start(&mut self, events: &mut NetEvents) -> Result<()> {
        let invite = (self.meta.role == Role::Creator)
            .then(|| Invite::new(self.meta.topic, self.meta.creator, &self.meta.relay).render())
            .transpose()?;
        let (epoch, members) = self.epoch_and_members();
        self.printer.ready(
            self.meta.role.as_str(),
            self.meta.status.as_str(),
            epoch,
            members,
            &self.meta.topic,
            invite.as_deref(),
        );

        // A joiner publishes exactly one KeyPackage, even across restarts.
        if self.meta.status == JoinStatus::Pending && !self.meta.key_package_published {
            let kp = self.mls.new_key_package()?;
            let envelope = Envelope::key_package(kp);
            tracing::info!(bytes = envelope.encoded_len(), "publishing KeyPackage");
            let hash = self.net.publish(&envelope).await?;
            self.state
                .record(&hash, Outcome::Ok, Some("own KeyPackage"), None)?;
            self.state.set_key_package_published()?;
            self.meta.key_package_published = true;
        }

        self.event_loop(events).await
    }

    async fn event_loop(&mut self, events: &mut NetEvents) -> Result<()> {
        let mut lines = spawn_stdin();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else { break };
                    match self.handle_line(&line).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) => {
                            self.printer.error(&format!("{e:#}"));
                            return Err(e);
                        }
                    }
                }
                event = events.next() => {
                    let Some(event) = event else {
                        self.printer.error("topic stream closed");
                        bail!("topic stream closed");
                    };
                    if let Err(e) = self.handle_event(event).await {
                        tracing::error!("event handling failed: {e:#}");
                        self.printer.error(&format!("{e:#}"));
                        return Err(e);
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
                _ = ticker.tick() => self.tick(),
            }
        }
        Ok(())
    }

    fn tick(&mut self) {
        if self.meta.status == JoinStatus::Pending
            && !self.hinted
            && self.started.elapsed() >= WELCOME_HINT_AFTER
        {
            self.hinted = true;
            self.printer.info(&format!(
                "No Welcome after {}s. Check that the creator ({}) is online and saw a join request; still waiting.",
                WELCOME_HINT_AFTER.as_secs(),
                short_id(&self.meta.creator)
            ));
        }
    }

    /// Returns false when the session should end.
    async fn handle_line(&mut self, line: &str) -> Result<bool> {
        if !self.asks.is_empty() && !line.starts_with('/') {
            let ask = self.asks.pop_front().expect("non-empty");
            let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
            self.answer(ask, yes).await?;
            return Ok(true);
        }
        if line.is_empty() {
            return Ok(true);
        }
        if let Some(cmd) = line.strip_prefix('/') {
            match cmd {
                "quit" => return Ok(false),
                "status" => self.print_status()?,
                "members" => self.print_members(),
                other => self.printer.error(&format!("unknown command /{other}")),
            }
            return Ok(true);
        }
        if self.meta.status != JoinStatus::Joined {
            self.printer
                .error("not joined yet; messages are disabled until the creator's Welcome arrives");
            return Ok(true);
        }
        if let Err(e) = ChatBody::new(line, 0) {
            self.printer.error(&e.to_string());
            return Ok(true);
        }
        self.send(line).await?;
        Ok(true)
    }

    fn print_status(&self) -> Result<()> {
        let c = self.state.counts()?;
        let (epoch, members) = self.epoch_and_members();
        if self.printer.json {
            self.printer.raw(serde_json::json!({
                "event": "status",
                "role": self.meta.role.as_str(),
                "status": self.meta.status.as_str(),
                "epoch": epoch,
                "members": members,
                "ok": c.ok,
                "skipped": c.skipped,
                "failed": c.failed,
            }));
        } else {
            let epoch = epoch.map(|e| e.to_string()).unwrap_or_else(|| "-".into());
            self.printer.info(&format!(
                "{} {} | epoch {epoch} | {members} members | ops ok={} skipped={} failed={}",
                self.meta.role.as_str(),
                self.meta.status.as_str(),
                c.ok,
                c.skipped,
                c.failed
            ));
        }
        Ok(())
    }

    fn print_members(&self) {
        let members = self.mls.members().unwrap_or_default();
        if self.printer.json {
            let ids: Vec<String> = members.iter().map(hex_id).collect();
            self.printer
                .raw(serde_json::json!({"event": "members", "members": ids}));
        } else if members.is_empty() {
            self.printer.info("no members yet");
        } else {
            for m in members {
                let tag = if m == self.me { " (me)" } else { "" };
                self.printer.info(&format!("{}{tag}", hex_id(&m)));
            }
        }
    }

    async fn send(&mut self, text: &str) -> Result<()> {
        let body = ChatBody::new(text, now_ms())?;
        let ciphertext = self.mls.encrypt(&body.encode()?)?;
        let envelope = Envelope::app(ciphertext);
        let hash = self.net.publish(&envelope).await?;
        // MLS cannot decrypt our own ciphertext: keep the plaintext now.
        let msg = StoredMessage {
            op_hash: hash,
            author: self.me,
            sent_at_ms: body.sent_at_ms,
            op_ts: None,
            text: body.text.clone(),
        };
        self.state.record(&hash, Outcome::Ok, None, Some(&msg))?;
        tracing::trace!(text = %body.text, "sent");
        self.printer
            .message(&self.me, &body.text, &hash, body.sent_at_ms);
        Ok(())
    }

    async fn handle_event(&mut self, event: NetEvent) -> Result<()> {
        match event {
            NetEvent::Op(op) => {
                let before = self.mls.epoch().ok();
                self.handle_op(*op).await?;
                if self.mls.epoch().ok() != before {
                    self.retry_deferred().await?;
                }
            }
            NetEvent::SyncStarted {
                peer,
                incoming_ops,
                incoming_bytes,
            } => {
                tracing::info!(peer = %short_id(&peer), incoming_ops, "sync started");
                self.printer
                    .sync_started(&peer, incoming_ops, incoming_bytes)
            }
            NetEvent::SyncEnded {
                peer,
                received_ops,
                sent_ops,
                error,
            } => {
                tracing::info!(peer = %short_id(&peer), received_ops, sent_ops, ?error, "sync ended");
                self.printer
                    .sync_ended(&peer, received_ops, sent_ops, error.as_deref())
            }
            NetEvent::ReplayStarted { total } => self
                .printer
                .verbose(&format!("replaying {total} unacknowledged operations")),
            NetEvent::ReplayEnded => self.printer.verbose("replay ended"),
            NetEvent::Failed { hash, error } => {
                tracing::warn!(hash = ?hash.map(hex::encode), "{error}");
                self.printer.verbose(&format!("p2panda: {error}"));
            }
            NetEvent::Other => {}
        }
        Ok(())
    }

    async fn handle_op(&mut self, op: Incoming) -> Result<()> {
        // Idempotency: look the hash up before any MLS call.
        if self.state.outcome(&op.hash)?.is_some() {
            self.state.set_op_ts(&op.hash, op.timestamp)?;
            ack(&op).await?;
            return Ok(());
        }
        if self.deferred.iter().any(|pending| pending.hash == op.hash) {
            return Ok(());
        }
        // A join request already waiting for an answer (delivered twice).
        if self.asks.iter().any(|a| a.op.hash == op.hash) {
            return Ok(());
        }
        tracing::debug!(
            op = %hex::encode(op.hash),
            author = %short_id(&op.author),
            kind = op.envelope.kind(),
            bytes = op.envelope.encoded_len(),
            source = op.source,
            "operation"
        );
        let view = self.view();
        let input = OpInput {
            author: op.author,
            hash: op.hash,
            timestamp: op.timestamp,
            envelope: &op.envelope,
        };
        let effects = group::handle(&view, &input, &mut self.mls, self.opts.auto_accept);
        if effects == [Effect::Defer] {
            if self.deferred.len() >= 256 {
                bail!(
                    "too many messages waiting for an MLS commit; restart to replay unacknowledged operations"
                );
            }
            self.deferred.push_back(op);
            return Ok(());
        }
        if let Some(Effect::AskUser { member }) = effects.first() {
            let member = *member;
            let Envelope::KeyPackage { key_package, .. } = &op.envelope else {
                bail!("AskUser for a non-KeyPackage envelope");
            };
            let key_package = key_package.to_vec();
            self.printer.join_request(&member, true);
            self.asks.push_back(PendingJoin {
                op,
                member,
                key_package,
            });
            return Ok(());
        }
        if matches!(op.envelope, Envelope::KeyPackage { .. })
            && effects.iter().any(|e| matches!(e, Effect::Joined { .. }))
        {
            self.printer.join_request(&op.author, false);
        }
        self.apply(&op, effects).await
    }

    async fn retry_deferred(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.deferred);
        for op in pending {
            self.handle_op(op).await?;
        }
        Ok(())
    }

    async fn answer(&mut self, ask: PendingJoin, yes: bool) -> Result<()> {
        let effects = if yes {
            let view = self.view();
            group::accept_join(&view, &ask.member, &ask.key_package, &mut self.mls)
        } else {
            self.printer
                .info(&format!("declined {}", short_id(&ask.member)));
            group::decline_join()
        };
        self.apply(&ask.op, effects).await?;
        self.retry_deferred().await
    }

    /// Applies effects in order: publish, persist, tell the user, acknowledge.
    async fn apply(&mut self, op: &Incoming, effects: Vec<Effect>) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Publish(envelope) => {
                    tracing::info!(
                        kind = envelope.kind(),
                        bytes = envelope.encoded_len(),
                        "publishing"
                    );
                    let hash = self.net.publish(&envelope).await?;
                    self.state.record(&hash, Outcome::Ok, Some("own"), None)?;
                }
                Effect::AskUser { .. } => {}
                Effect::Defer => bail!("deferred operation reached effect execution"),
                Effect::SetJoined { epoch } => {
                    self.state.set_joined(epoch)?;
                    self.meta.status = JoinStatus::Joined;
                    self.meta.joined_epoch = Some(epoch);
                }
                Effect::Joined {
                    member,
                    epoch,
                    members,
                } => {
                    tracing::info!(member = %short_id(&member), epoch, members, "joined");
                    self.printer.joined(&member, epoch, members);
                }
                Effect::EpochChanged { epoch, members } => {
                    tracing::info!(epoch, members, "epoch changed");
                    self.printer.info(&format!(
                        "group is now at epoch {epoch} with {members} members"
                    ));
                }
                Effect::Record {
                    outcome,
                    reason,
                    message,
                } => {
                    self.state
                        .record(&op.hash, outcome, reason.as_deref(), message.as_ref())?;
                    if let Some(m) = &message {
                        tracing::trace!(text = %m.text, "received");
                        self.printer
                            .message(&m.author, &m.text, &m.op_hash, op.timestamp / 1000);
                    }
                    match outcome {
                        Outcome::Ok => {}
                        Outcome::Skipped => tracing::info!(
                            op = %hex::encode(op.hash),
                            kind = op.envelope.kind(),
                            "skipped: {}",
                            reason.as_deref().unwrap_or("")
                        ),
                        Outcome::Failed => {
                            tracing::warn!(
                                op = %hex::encode(op.hash),
                                kind = op.envelope.kind(),
                                "failed: {}",
                                reason.as_deref().unwrap_or("")
                            );
                            self.printer.verbose(&format!(
                                "operation {} failed: {}",
                                &hex::encode(op.hash)[..8],
                                reason.as_deref().unwrap_or("")
                            ));
                        }
                    }
                }
                Effect::Ack => ack(op).await?,
            }
        }
        Ok(())
    }
}
