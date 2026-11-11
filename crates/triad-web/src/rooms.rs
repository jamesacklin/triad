use std::{future::pending, path::PathBuf, sync::Arc};

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
use triad_core::{
    envelope::{ChatBody, Invite},
    identity::{Identity, NodeId},
    net::{DEFAULT_RELAY, P2PANDA_FILE},
    state::{JoinStatus, Meta, Outcome, Role, State, StoredMessage, channel_name},
};

pub enum Start {
    Create,
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
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Message {
    author: String,
    text: String,
    mine: bool,
}

struct Send {
    body: ChatBody,
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
        };
        if let Some(meta) = &meta {
            view.room(meta)?;
            view.messages = state
                .history(&meta.topic, None)?
                .into_iter()
                .map(|message| Message::from_stored(message, identity.node_id()))
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

    pub async fn send(&self, body: ChatBody) -> Result<()> {
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
        let (relay, bootstrap) = match &start {
            Start::Create => (DEFAULT_RELAY.to_owned(), None),
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
        self.run_room(&mut store, room, commands).await
    }

    async fn run_room(
        &self,
        store: &mut State,
        room: Room,
        mut commands: mpsc::Receiver<Send>,
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
        loop {
            tokio::select! {
                result = &mut joined, if waiting => {
                    space = Some(result?);
                    waiting = false;
                    store.set_joined(&meta.topic)?;
                    self.session.lock().await.view.status = "joined";
                }
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()) };
                    let _ = command.result.send(publish(space.as_ref(), command.body).await);
                }
                event = events.next() => {
                    match event {
                        Some(StreamEvent::Processed { operation, .. }) => {
                            let hash = *operation.id().as_bytes();
                            if store.outcome(&hash)?.is_none() {
                                let message = StoredMessage {
                                    op_hash: hash,
                                    author: *operation.author().as_bytes(),
                                    sent_at_ms: operation.message().sent_at_ms,
                                    op_ts: Some(operation.timestamp()),
                                    text: operation.message().text.clone(),
                                };
                                store.record(&meta.topic, &hash, Outcome::Ok, None, Some(&message))?;
                                self.session.lock().await.view.messages.push(Message::from_stored(message, me));
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

struct Room {
    meta: Meta,
    events: SpaceSubscription<ChatBody>,
    space: Option<Space<ChatBody>>,
    joining: Option<PendingSpace<ChatBody>>,
}

async fn open_room(
    node: &Node,
    store: &mut State,
    start: Start,
    me: NodeId,
    relay: String,
) -> Result<Room> {
    match start {
        Start::Create => {
            let (space, events) = node.create_space::<ChatBody>().await?;
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
            let (joining, events) = node.space::<ChatBody>(Hash::from(meta.topic)).await?;
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
            let (joining, events) = node.space::<ChatBody>(Hash::from(meta.topic)).await?;
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

async fn publish(space: Option<&Space<ChatBody>>, body: ChatBody) -> Result<()> {
    let space = space.ok_or_else(|| anyhow!("room is not connected"))?;
    space.publish(body).await?.await?;
    Ok(())
}

impl View {
    fn room(&mut self, meta: &Meta) -> Result<()> {
        self.group = hex::encode(meta.topic);
        if meta.role == Role::Creator {
            self.invite = Invite::new(meta.topic, meta.creator, &meta.relay)
                .with_name(&meta.name)
                .render()?;
        }
        Ok(())
    }
}

impl Message {
    fn from_stored(message: StoredMessage, me: NodeId) -> Self {
        Self {
            author: hex::encode(message.author),
            text: message.text,
            mine: message.author == me,
        }
    }
}
