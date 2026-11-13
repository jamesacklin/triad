//! The hub: one task that owns the table of connected clients, numbers
//! every frame (`seq`) and routes each event to every client or to the one
//! that asked. A client whose queue is full is cut loose (`lagged`).

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::proto::{self, ClientId, Target};

/// Frames waiting to be written to one client before it counts as lagging.
pub const QUEUE: usize = 1024;

/// What the session's printer and the connection tasks tell the hub.
pub enum HubMsg {
    /// A client authenticated. The hub sends it `hello` first.
    Join {
        client: ClientId,
        frames: mpsc::Sender<String>,
        /// Fired with the `seq` of the first frame the client will not get
        /// when its queue overflows.
        lagged: oneshot::Sender<u64>,
    },
    /// A connection ended.
    Leave(ClientId),
    /// An event. `target` is set while a request (or a connect) is being
    /// handled.
    Event {
        value: Value,
        target: Option<Target>,
    },
}

pub type Hub = mpsc::UnboundedSender<HubMsg>;

struct Client {
    frames: mpsc::Sender<String>,
    lagged: Option<oneshot::Sender<u64>>,
}

/// Starts the hub. `node` is this node's ID for `hello`.
pub fn spawn(node: String, version: &'static str) -> Hub {
    let (tx, mut rx) = mpsc::unbounded_channel::<HubMsg>();
    tokio::spawn(async move {
        let mut clients: BTreeMap<ClientId, Client> = BTreeMap::new();
        let mut seq: u64 = 0;
        while let Some(msg) = rx.recv().await {
            match msg {
                HubMsg::Join {
                    client,
                    frames,
                    lagged,
                } => {
                    clients.insert(
                        client,
                        Client {
                            frames,
                            lagged: Some(lagged),
                        },
                    );
                    seq += 1;
                    let hello = json!({"event": "hello", "protocol": proto::PROTOCOL,
                                       "version": version, "node": node, "client": client});
                    deliver(&mut clients, client, frame(hello, None, seq), seq);
                }
                HubMsg::Leave(client) => {
                    clients.remove(&client);
                }
                HubMsg::Event { value, target } => {
                    seq += 1;
                    let event = value["event"].as_str().unwrap_or_default();
                    match target {
                        None => {
                            let text = frame(value, None, seq);
                            let ids: Vec<ClientId> = clients.keys().copied().collect();
                            for id in ids {
                                deliver(&mut clients, id, text.clone(), seq);
                            }
                        }
                        Some(t) if proto::broadcast(event) => {
                            // The asker's copy carries its `re`; nobody
                            // else's does.
                            let mine = frame(value.clone(), t.re.as_ref(), seq);
                            let others = frame(value, None, seq);
                            let ids: Vec<ClientId> = clients.keys().copied().collect();
                            for id in ids {
                                let text = if id == t.client {
                                    mine.clone()
                                } else {
                                    others.clone()
                                };
                                deliver(&mut clients, id, text, seq);
                            }
                        }
                        Some(t) => {
                            let text = frame(value, t.re.as_ref(), seq);
                            deliver(&mut clients, t.client, text, seq);
                        }
                    }
                }
            }
        }
    });
    tx
}

/// The frame's text: the event with `re` (when answering a request) and
/// `seq` added.
fn frame(mut value: Value, re: Option<&Value>, seq: u64) -> String {
    if let Some(fields) = value.as_object_mut() {
        if let Some(re) = re {
            fields.insert("re".into(), re.clone());
        }
        fields.insert("seq".into(), json!(seq));
    }
    value.to_string()
}

fn deliver(clients: &mut BTreeMap<ClientId, Client>, id: ClientId, text: String, seq: u64) {
    let Some(client) = clients.get_mut(&id) else {
        return;
    };
    match client.frames.try_send(text) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::info!(client = id, seq, "websocket client lagged; closing it");
            if let Some(lagged) = client.lagged.take() {
                let _ = lagged.send(seq);
            }
            clients.remove(&id);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            clients.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn join(
        hub: &Hub,
        client: ClientId,
        cap: usize,
    ) -> (mpsc::Receiver<String>, oneshot::Receiver<u64>) {
        let (frames, rx) = mpsc::channel(cap);
        let (lagged, lag_rx) = oneshot::channel();
        hub.send(HubMsg::Join {
            client,
            frames,
            lagged,
        })
        .unwrap();
        (rx, lag_rx)
    }

    fn parse(s: String) -> Value {
        serde_json::from_str(&s).unwrap()
    }

    #[tokio::test]
    async fn routes_replies_to_the_asker_and_changes_to_everyone() {
        let hub = spawn("aa".into(), "0");
        let (mut a, _) = join(&hub, 1, 16).await;
        let (mut b, _) = join(&hub, 2, 16).await;
        assert_eq!(parse(a.recv().await.unwrap())["event"], "hello");
        assert_eq!(parse(b.recv().await.unwrap())["event"], "hello");
        let ask = Some(Target {
            client: 1,
            re: Some(json!(9)),
        });
        hub.send(HubMsg::Event {
            value: json!({"event": "members"}),
            target: ask.clone(),
        })
        .unwrap();
        hub.send(HubMsg::Event {
            value: json!({"event": "message"}),
            target: ask.clone(),
        })
        .unwrap();
        hub.send(HubMsg::Event {
            value: json!({"event": "brand_new_query"}),
            target: ask,
        })
        .unwrap();
        let a1 = parse(a.recv().await.unwrap());
        assert_eq!(
            (a1["event"].as_str(), &a1["re"]),
            (Some("members"), &json!(9))
        );
        let a2 = parse(a.recv().await.unwrap());
        assert_eq!(
            (a2["event"].as_str(), &a2["re"]),
            (Some("message"), &json!(9))
        );
        let a3 = parse(a.recv().await.unwrap());
        assert_eq!(a3["event"], "brand_new_query");
        let b1 = parse(b.recv().await.unwrap());
        assert_eq!(b1["event"], "message");
        assert!(b1.get("re").is_none(), "{b1}");
        assert_eq!(b1["seq"], a2["seq"]);
        assert!(a1["seq"].as_u64() < a2["seq"].as_u64());
    }

    #[tokio::test]
    async fn a_full_queue_is_cut_loose() {
        let hub = spawn("aa".into(), "0");
        let (_a, lagged) = join(&hub, 1, 2).await;
        for _ in 0..3 {
            hub.send(HubMsg::Event {
                value: json!({"event": "message"}),
                target: None,
            })
            .unwrap();
        }
        // hello and one message fit; the second message does not.
        assert_eq!(lagged.await.unwrap(), 3);
    }
}
