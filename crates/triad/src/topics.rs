//! Topics: sub-channels opened under a channel with `/topic <name>`, or by
//! the channel's bot with `channel.topic`.
//!
//! p2panda has no nesting, so a topic is a space of its own. Only
//! `state.sqlite` files it under its channel (`Meta::parent`), and its
//! local name is the channel's and its own joined by `/`
//! (`design/ideas`).
//!
//! Membership follows the channel. The topic's creator announces it in the
//! channel ([`ChatBody::topic`]); a member's client that reads the
//! announcement from someone with `Manage` access there joins the topic,
//! which announces its key bundle; and a manager of the topic lets in,
//! without asking, anyone who is a member of the channel, at the access
//! they have there. Someone removed from the channel is removed from its
//! topics by whichever of their managers sees it. One level only: a topic
//! has no topics.

use anyhow::{Context, Result, bail};
use p2panda::AccessLevel;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use triad_agent::channel::Channels;
use triad_agent::tools::BoxFuture;
use triad_core::envelope::{ChatBody, TopicRef};
use triad_core::identity::{NodeId, hex_id, short_id};
use triad_core::state::{JoinStatus, Meta, Role, now_ms, topic_name};

use crate::{Session, Streams, Topic, key, relay_url};

/// A bot turn's request to open a topic, answered on `reply`.
pub struct TopicAsk {
    channel: String,
    name: String,
    reply: oneshot::Sender<Result<String, String>>,
}

/// [`Channels`] for the bots: sends each request to the event loop, which
/// owns the spaces.
pub struct Opener(pub mpsc::UnboundedSender<TopicAsk>);

impl Channels for Opener {
    fn open_topic<'a>(
        &'a self,
        channel: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            self.0
                .send(TopicAsk {
                    channel: channel.to_string(),
                    name: name.to_string(),
                    reply,
                })
                .map_err(|_| "the session is closing".to_string())?;
            answer
                .await
                .map_err(|_| "the session is closing".to_string())?
        })
    }
}

impl Session {
    /// Opens a topic under `parent` and announces it there. Needs `Manage`
    /// access in `parent`. A channel whose bot is mine gets my bot in the
    /// topic too.
    pub(crate) async fn open_topic(
        &mut self,
        parent: Topic,
        name: &str,
        streams: &mut Streams,
    ) -> Result<Topic> {
        let own = topic_name(name)?;
        if self.is_dm(&parent) {
            bail!("your bot DM has no topics");
        }
        let c = self.channel(&parent)?;
        let parent_name = c.meta.name.clone();
        if let Some(p) = c.meta.parent {
            bail!(
                "#{parent_name} is a topic; open topics in #{}",
                self.name(&p)
            );
        }
        if c.meta.status != JoinStatus::Joined {
            bail!("not a member of #{parent_name} yet");
        }
        if !self.can_manage(&parent).await {
            bail!("only a member with Manage access in #{parent_name} can open topics there");
        }
        let topic = self
            .create_space(format!("{parent_name}/{own}"), Some(parent), None, streams)
            .await?;
        let body =
            ChatBody::new(&format!("opened a topic: {own}"), now_ms())?.with_topic(TopicRef {
                name: own,
                space: topic,
                relay: self.channel(&topic)?.meta.relay.clone(),
            });
        self.publish_chat(parent, body).await?;
        if self.hosting(&parent)
            && let Err(e) = self.bot_on(topic, streams).await
        {
            self.printer.error(&format!(
                "my bot could not join #{}: {e:#}",
                self.name(&topic)
            ));
        }
        Ok(topic)
    }

    /// A bot turn's `channel.topic`.
    pub(crate) async fn on_topic_ask(&mut self, ask: TopicAsk, streams: &mut Streams) {
        let result = async {
            let parent = self.resolve(&ask.channel)?;
            let topic = self.open_topic(parent, &ask.name, streams).await?;
            let name = self.name(&topic).to_string();
            self.emit_channel(topic, format!("my bot opened #{name}"))?;
            anyhow::Ok(name)
        }
        .await
        .map_err(|e| format!("{e:#}"));
        let _ = ask.reply.send(result);
    }

    /// Reports a channel added without moving the focus.
    fn emit_channel(&self, topic: Topic, text: String) -> Result<()> {
        let mut value = self.channel_json(self.channel(&topic)?)?;
        value["event"] = json!("channel");
        self.printer.emit(value, || text);
        Ok(())
    }

    /// A topic announced in `parent`: joins it if `author` may open topics
    /// there.
    pub(crate) async fn on_topic_announced(
        &mut self,
        parent: Topic,
        author: NodeId,
        sub: &TopicRef,
        streams: &mut Streams,
    ) -> Result<()> {
        if author == self.me || self.is_dm(&parent) || self.channels.contains_key(&sub.space) {
            return Ok(());
        }
        let Some(c) = self.channels.get(&parent) else {
            return Ok(());
        };
        if c.meta.parent.is_some() {
            return Ok(());
        }
        let parent_name = c.meta.name.clone();
        self.refresh_members(parent).await;
        let manages = self
            .channel(&parent)?
            .members
            .get(&author)
            .is_some_and(|a| *a == AccessLevel::Manage);
        if !manages {
            self.printer.info(&format!(
                "[#{parent_name}] topic from {} not joined: they cannot manage #{parent_name}",
                short_id(&author)
            ));
            return Ok(());
        }
        let Ok(own) = topic_name(&sub.name) else {
            self.printer.info(&format!(
                "[#{parent_name}] topic from {} not joined: its name is not usable",
                short_id(&author)
            ));
            return Ok(());
        };
        let taken = |n: &str| self.channels.values().any(|c| c.meta.name == n);
        let mut name = format!("{parent_name}/{own}");
        if taken(&name) {
            let own: String = own.chars().take(23).collect();
            name = format!("{parent_name}/{own}-{}", hex::encode(&sub.space[..4]));
        }
        if let Ok(relay) = relay_url(&sub.relay) {
            // Best effort: the author is reachable through the channel too.
            let _ = self.node.insert_bootstrap(key(&author)?, relay).await;
        }
        let topic = self
            .join_space(
                Meta {
                    name: name.clone(),
                    topic: sub.space,
                    creator: author,
                    relay: sub.relay.clone(),
                    role: Role::Member,
                    status: JoinStatus::Pending,
                    parent: Some(parent),
                },
                streams,
            )
            .await
            .with_context(|| format!("joining the topic #{name}"))?;
        self.emit_channel(
            topic,
            format!(
                "[#{parent_name}] {} opened #{name}; joining",
                short_id(&author)
            ),
        )
    }

    /// A join request in a topic this node manages: lets `member` in, at
    /// the access they have in the topic's channel, if they are a member
    /// there. Returns whether it did.
    pub(crate) async fn let_into_topic(&mut self, topic: Topic, member: NodeId) -> Result<bool> {
        let Some(parent) = self.channels.get(&topic).and_then(|c| c.meta.parent) else {
            return Ok(false);
        };
        self.refresh_members(parent).await;
        let Some(access) = self
            .channels
            .get(&parent)
            .and_then(|c| c.members.get(&member))
            .copied()
        else {
            return Ok(false);
        };
        let (name, parent_name) = (
            self.name(&topic).to_string(),
            self.name(&parent).to_string(),
        );
        self.printer.emit(
            json!({"event": "join_request", "channel": name, "member": hex_id(&member),
                   "auto": true}),
            || {
                format!(
                    "[#{name}] letting in {}, a member of #{parent_name}",
                    short_id(&member)
                )
            },
        );
        self.joined_space(&topic)?
            .add(key(&member)?, access)
            .await?;
        tracing::info!(member = %short_id(&member), channel = %name, "added topic member");
        Ok(true)
    }

    /// Members gone from `parent` leave its topics too, where this node
    /// manages them. Nothing happens when this node is among them: it can
    /// no longer tell who is still in.
    pub(crate) async fn remove_from_topics(&mut self, parent: Topic, gone: &[NodeId]) {
        if gone.is_empty() || gone.contains(&self.me) {
            return;
        }
        let topics: Vec<Topic> = self
            .channels
            .values()
            .filter(|c| c.meta.parent == Some(parent) && c.space.is_some())
            .map(|c| c.meta.topic)
            .collect();
        for topic in topics {
            if !self.can_manage(&topic).await {
                continue;
            }
            self.refresh_members(topic).await;
            for member in gone {
                let Some(c) = self.channels.get(&topic) else {
                    break;
                };
                if !c.members.contains_key(member) {
                    continue;
                }
                let Some(space) = &c.space else { break };
                let removed = match key(member) {
                    Ok(k) => space.remove(k).await.map_err(anyhow::Error::from),
                    Err(e) => Err(e),
                };
                match removed {
                    Ok(()) => {
                        tracing::info!(member = %short_id(member), channel = %c.meta.name, "removed from topic")
                    }
                    Err(e) => self.printer.error(&format!(
                        "removing {} from #{}: {e:#}",
                        short_id(member),
                        c.meta.name
                    )),
                }
            }
        }
    }
}
