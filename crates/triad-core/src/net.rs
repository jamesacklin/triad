//! p2panda wrapper: node builder, topic stream, publish, acknowledge. No
//! p2panda type appears in this module's public signatures.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use p2panda::node::AckPolicy;
use p2panda::streams::{
    ProcessedOperation, Source, StreamEvent, StreamPublisher, StreamSubscription,
};
use p2panda::{Node, RelayUrl, Topic, VerifyingKey};
use p2panda_core::traits::Digest;
use tokio_stream::StreamExt;

use crate::envelope::Envelope;
use crate::identity::{Identity, NodeId};

pub const P2PANDA_FILE: &str = "p2panda.sqlite";

/// The default public relay, one of those listed in iroh 1.4.1's `src/defaults.rs`.
pub const DEFAULT_RELAY: &str = "https://use1-1.relay.n0.iroh.link./";

pub struct NetConfig<'a> {
    pub data_dir: &'a Path,
    pub relay: &'a str,
    /// Node to bootstrap discovery from (the creator, for joiners).
    pub bootstrap: Option<NodeId>,
    pub topic: [u8; 32],
}

/// An operation delivered on the topic, ready for the app to process.
pub struct Incoming {
    pub author: NodeId,
    pub hash: [u8; 32],
    /// p2panda operation timestamp, microseconds since the UNIX epoch.
    pub timestamp: u64,
    pub envelope: Envelope,
    /// Where the operation came from: "sync", "publish", "replay", ...
    pub source: &'static str,
    ack: ProcessedOperation<Envelope>,
}

impl Incoming {
    /// Acknowledges the operation so it is not replayed on the next start.
    pub async fn ack(&self) -> Result<()> {
        self.ack.ack().await.map_err(|e| anyhow!("ack failed: {e}"))
    }
}

/// Everything the topic subscription can report, reduced to what the app needs.
pub enum NetEvent {
    Op(Box<Incoming>),
    SyncStarted {
        peer: NodeId,
        incoming_ops: u32,
        incoming_bytes: u32,
    },
    SyncEnded {
        peer: NodeId,
        received_ops: u32,
        sent_ops: u32,
        error: Option<String>,
    },
    ReplayStarted {
        total: u32,
    },
    ReplayEnded,
    /// An operation failed somewhere below the app (processing, decoding, ack, replay).
    Failed {
        hash: Option<[u8; 32]>,
        error: String,
    },
    /// Other p2panda events the app ignores.
    Other,
}

pub struct Net {
    node: Node,
    publisher: StreamPublisher<Envelope>,
}

pub struct NetEvents {
    sub: StreamSubscription<Envelope>,
}

impl NetEvents {
    pub async fn next(&mut self) -> Option<NetEvent> {
        let event = self.sub.next().await?;
        Some(convert(event))
    }
}

impl Net {
    /// Spawns the p2panda node with explicit acks and subscribes to the topic.
    pub async fn start(identity: &Identity, cfg: NetConfig<'_>) -> Result<(Net, NetEvents)> {
        let relay: RelayUrl = cfg.relay.parse().context("invalid relay URL")?;
        let db_path = cfg.data_dir.join(P2PANDA_FILE);
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let mut builder = p2panda::builder()
            .credentials(identity.credentials.clone())
            .database_url(&db_url)
            .ack_policy(AckPolicy::Explicit)
            .relay_url(relay.clone());
        if let Some(bootstrap) = cfg.bootstrap {
            let key = VerifyingKey::from_bytes(&bootstrap).context("invalid bootstrap node ID")?;
            builder = builder.bootstrap(key, relay);
        }
        let node = builder.spawn().await.context("spawning p2panda node")?;
        let (publisher, sub) = node
            .stream::<Envelope>(Topic::from(cfg.topic))
            .await
            .context("subscribing to topic")?;
        Ok((Net { node, publisher }, NetEvents { sub }))
    }

    pub fn node_id(&self) -> NodeId {
        *self.node.id().as_bytes()
    }

    /// Publishes an envelope and returns the operation hash.
    pub async fn publish(&self, envelope: &Envelope) -> Result<[u8; 32]> {
        envelope.check_size()?;
        let fut = self
            .publisher
            .publish(envelope.clone())
            .await
            .map_err(|e| anyhow!("publish failed: {e}"))?;
        Ok(*fut.hash().as_bytes())
    }

    /// Publishes without the envelope size check (for payload measurements).
    pub async fn publish_unchecked(&self, envelope: &Envelope) -> Result<[u8; 32]> {
        let fut = self
            .publisher
            .publish(envelope.clone())
            .await
            .map_err(|e| anyhow!("publish failed: {e}"))?;
        Ok(*fut.hash().as_bytes())
    }

    /// Closes the topic's sync session and drops the node.
    pub async fn shutdown(self) {
        if let Err(e) = self.publisher.close().await {
            tracing::warn!("closing stream: {e}");
        }
        drop(self.publisher);
        drop(self.node);
    }
}

fn source_name(source: &Source) -> &'static str {
    match source {
        Source::SyncSession { .. } => "sync",
        Source::ExternalStream { .. } => "import",
        Source::Publish => "publish",
        Source::Replay => "replay",
        Source::Egress => "egress",
    }
}

fn convert(event: StreamEvent<Envelope>) -> NetEvent {
    match event {
        StreamEvent::Processed { operation, source } => NetEvent::Op(Box::new(Incoming {
            author: *operation.author().as_bytes(),
            hash: *operation.id().as_bytes(),
            timestamp: operation.timestamp(),
            envelope: operation.message().clone(),
            source: source_name(&source),
            ack: operation,
        })),
        StreamEvent::SyncStarted {
            remote_node_id,
            incoming_operations,
            incoming_bytes,
            ..
        } => NetEvent::SyncStarted {
            peer: *remote_node_id.as_bytes(),
            incoming_ops: incoming_operations,
            incoming_bytes,
        },
        StreamEvent::SyncEnded {
            remote_node_id,
            received_operations,
            sent_operations,
            error,
            ..
        } => NetEvent::SyncEnded {
            peer: *remote_node_id.as_bytes(),
            received_ops: received_operations,
            sent_ops: sent_operations,
            error: error.map(|e| e.to_string()),
        },
        StreamEvent::ReplayStarted { total_operations } => NetEvent::ReplayStarted {
            total: total_operations,
        },
        StreamEvent::ReplayEnded => NetEvent::ReplayEnded,
        StreamEvent::ProcessingFailed { event, error, .. } => NetEvent::Failed {
            hash: Some(*event.hash().as_bytes()),
            error: format!("processing failed: {error}"),
        },
        StreamEvent::DecodeFailed { event, error } => NetEvent::Failed {
            hash: Some(*event.hash().as_bytes()),
            error: format!("decode failed: {error}"),
        },
        StreamEvent::AckFailed { event, error } => NetEvent::Failed {
            hash: Some(*event.hash().as_bytes()),
            error: format!("ack failed: {error}"),
        },
        StreamEvent::ReplayFailed { error } => NetEvent::Failed {
            hash: None,
            error: format!("replay failed: {error}"),
        },
        _ => NetEvent::Other,
    }
}

/// A fresh random topic ID.
pub fn random_topic() -> [u8; 32] {
    Topic::random().to_bytes()
}
