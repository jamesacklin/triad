//! Chat mode: stdin lines in, events out. Milestone 2 version: `Envelope::App`
//! carries the CBOR `ChatBody` in plaintext and there is no MLS yet.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use triad_core::envelope::{ChatBody, Envelope, Invite, VERSION};
use triad_core::identity::{Identity, NodeId, hex_id, short_id};
use triad_core::net::{Incoming, Net, NetConfig, NetEvent, NetEvents, random_topic};
use triad_core::state::{JoinStatus, Meta, Outcome, Role, State, StoredMessage, now_ms};

use crate::output::Printer;

#[derive(Clone, Copy)]
pub struct Options {
    #[allow(dead_code)] // Used once the join flow lands (Milestone 4).
    pub auto_accept: bool,
    pub json: bool,
    pub verbose: bool,
}

pub fn mls_summary(
    _data_dir: &Path,
    _identity: &Identity,
    _meta: &Meta,
) -> Result<Option<(u64, Vec<NodeId>)>> {
    Ok(None)
}

pub async fn create(data_dir: &Path, relay: &str, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    let topic = random_topic();
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
    tracing::info!("created group");
    run(data_dir, identity, state, opts).await
}

pub async fn join(data_dir: &Path, invite: Invite, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    if invite.creator == identity.node_id() {
        bail!("this invite was created by this data directory");
    }
    let meta = Meta {
        topic: invite.topic,
        group_id: invite.topic.to_vec(),
        creator: invite.creator,
        relay: invite.relay.clone(),
        role: Role::Member,
        status: JoinStatus::Joined,
        key_package_published: false,
        joined_epoch: None,
    };
    state.insert_meta(&meta)?;
    run(data_dir, identity, state, opts).await
}

pub async fn resume(data_dir: &Path, opts: Options) -> Result<()> {
    let identity = Identity::load_or_create(data_dir)?;
    let state = State::open(data_dir)?;
    if state.meta()?.is_none() {
        bail!("no group in this data directory; run `triad create` or `triad join`");
    }
    run(data_dir, identity, state, opts).await
}

struct Session {
    me: NodeId,
    state: State,
    meta: Meta,
    net: Net,
    printer: Printer,
}

async fn run(data_dir: &Path, identity: Identity, state: State, opts: Options) -> Result<()> {
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
    let invite = (meta.role == Role::Creator)
        .then(|| Invite::new(meta.topic, meta.creator, &meta.relay).render())
        .transpose()?;
    printer.ready(
        meta.role.as_str(),
        meta.status.as_str(),
        None,
        0,
        &meta.topic,
        invite.as_deref(),
    );
    let mut session = Session {
        me,
        state,
        meta,
        net,
        printer,
    };
    let result = session.event_loop(&mut events).await;
    session.net.shutdown().await;
    result
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
    async fn event_loop(&mut self, events: &mut NetEvents) -> Result<()> {
        let mut lines = spawn_stdin();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else { break };
                    if !self.handle_line(&line).await? {
                        break;
                    }
                }
                event = events.next() => {
                    let Some(event) = event else {
                        self.printer.error("topic stream closed");
                        break;
                    };
                    if let Err(e) = self.handle_event(event).await {
                        tracing::error!("event handling failed: {e:#}");
                        self.printer.error(&format!("{e:#}"));
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
                _ = ticker.tick() => {}
            }
        }
        Ok(())
    }

    /// Returns false when the session should end.
    async fn handle_line(&mut self, line: &str) -> Result<bool> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(true);
        }
        if let Some(cmd) = line.strip_prefix('/') {
            match cmd {
                "quit" => return Ok(false),
                "status" => {
                    let c = self.state.counts()?;
                    self.printer.info(&format!(
                        "role {} status {} ops ok={} skipped={} failed={}",
                        self.meta.role.as_str(),
                        self.meta.status.as_str(),
                        c.ok,
                        c.skipped,
                        c.failed
                    ));
                }
                "members" => self.printer.info("members: (no MLS group in this build)"),
                other => self.printer.error(&format!("unknown command /{other}")),
            }
            return Ok(true);
        }
        if let Err(e) = self.send(line).await {
            self.printer.error(&format!("{e:#}"));
        }
        Ok(true)
    }

    async fn send(&mut self, text: &str) -> Result<()> {
        let body = ChatBody::new(text, now_ms())?;
        let envelope = Envelope::app(body.encode()?);
        let hash = self.net.publish(&envelope).await?;
        let msg = StoredMessage {
            op_hash: hash,
            author: self.me,
            sent_at_ms: body.sent_at_ms,
            op_ts: None,
            text: body.text.clone(),
        };
        self.state.record(&hash, Outcome::Ok, None, Some(&msg))?;
        self.printer
            .message(&self.me, &body.text, &hash, body.sent_at_ms);
        Ok(())
    }

    async fn handle_event(&mut self, event: NetEvent) -> Result<()> {
        match event {
            NetEvent::Op(op) => self.handle_op(*op).await?,
            NetEvent::SyncStarted {
                peer,
                incoming_ops,
                incoming_bytes,
            } => self
                .printer
                .sync_started(&peer, incoming_ops, incoming_bytes),
            NetEvent::SyncEnded {
                peer,
                received_ops,
                sent_ops,
                error,
            } => self
                .printer
                .sync_ended(&peer, received_ops, sent_ops, error.as_deref()),
            NetEvent::ReplayStarted { total } => self
                .printer
                .verbose(&format!("replaying {total} unacknowledged operations")),
            NetEvent::ReplayEnded => self.printer.verbose("replay ended"),
            NetEvent::Failed { hash, error } => {
                tracing::warn!(?hash, "{error}");
                self.printer.verbose(&format!("p2panda: {error}"));
            }
            NetEvent::Other => {}
        }
        Ok(())
    }

    async fn handle_op(&mut self, op: Incoming) -> Result<()> {
        if self.state.outcome(&op.hash)?.is_some() {
            // Already processed (our own publish, or a replay after a crash).
            self.state.set_op_ts(&op.hash, op.timestamp)?;
            op.ack().await?;
            return Ok(());
        }
        tracing::debug!(
            op = %hex::encode(op.hash),
            author = %short_id(&op.author),
            kind = op.envelope.kind(),
            source = op.source,
            "operation"
        );
        let (outcome, reason, message) = match &op.envelope {
            Envelope::App { v, ciphertext } if *v == VERSION => {
                match ChatBody::decode(ciphertext) {
                    Ok(body) => (
                        Outcome::Ok,
                        None,
                        Some(StoredMessage {
                            op_hash: op.hash,
                            author: op.author,
                            sent_at_ms: body.sent_at_ms,
                            op_ts: Some(op.timestamp),
                            text: body.text,
                        }),
                    ),
                    Err(e) => (Outcome::Skipped, Some(format!("bad body: {e}")), None),
                }
            }
            other => (
                Outcome::Skipped,
                Some(format!("unexpected {} envelope", other.kind())),
                None,
            ),
        };
        self.state
            .record(&op.hash, outcome, reason.as_deref(), message.as_ref())?;
        if let Some(m) = &message {
            self.printer
                .message(&m.author, &m.text, &m.op_hash, op.timestamp / 1000);
        } else if let Some(r) = &reason {
            tracing::info!(author = %hex_id(&op.author), "skipped operation: {r}");
        }
        op.ack().await?;
        let _ = &self.meta;
        Ok(())
    }
}
