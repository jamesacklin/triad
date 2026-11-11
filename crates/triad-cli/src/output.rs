//! Text and JSON rendering of chat events.

use serde_json::{Value, json};
use triad_core::identity::{NodeId, hex_id, short_id};

/// UTC wall clock as HH:MM:SS.
pub fn clock(ms: u64) -> String {
    let secs = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

pub struct Printer {
    pub json: bool,
    pub verbose: bool,
    pub me: NodeId,
}

impl Printer {
    fn emit(&self, value: Value) {
        println!("{value}");
    }

    pub fn ready(
        &self,
        role: &str,
        status: &str,
        epoch: Option<u64>,
        members: usize,
        topic: &[u8; 32],
        invite: Option<&str>,
    ) {
        if self.json {
            self.emit(json!({
                "event": "ready",
                "node": hex_id(&self.me),
                "role": role,
                "status": status,
                "epoch": epoch,
                "members": members,
                "group": hex::encode(topic),
                "invite": invite,
            }));
            return;
        }
        println!("node    {}", short_id(&self.me));
        let epoch = epoch.map(|e| e.to_string()).unwrap_or_else(|| "-".into());
        let plural = if members == 1 { "" } else { "s" };
        println!(
            "group   {} (epoch {epoch}, {members} member{plural})",
            hex::encode(&topic[..4])
        );
        if let Some(invite) = invite {
            println!("invite  {invite}");
        }
        if status == "pending" {
            println!("Waiting for the creator to accept. Messages are disabled until then.");
        } else {
            println!("Waiting for peers. Type a message, or /quit.");
        }
    }

    #[allow(dead_code)] // Used once the join flow lands (Milestone 4).
    pub fn join_request(&self, member: &NodeId, ask: bool) {
        if self.json {
            self.emit(json!({"event": "join_request", "member": hex_id(member)}));
        } else if ask {
            println!("join request from {}. Accept? [y/N]", short_id(member));
        } else {
            println!("join request from {}. Accepting.", short_id(member));
        }
    }

    #[allow(dead_code)] // Used once the join flow lands (Milestone 4).
    pub fn joined(&self, member: &NodeId, epoch: u64, members: usize) {
        if self.json {
            self.emit(json!({
                "event": "joined",
                "member": hex_id(member),
                "epoch": epoch,
                "members": members,
            }));
        } else {
            let who = if *member == self.me {
                "you".to_string()
            } else {
                short_id(member)
            };
            println!("{who} joined (epoch {epoch}, {members} members)");
        }
    }

    pub fn message(&self, author: &NodeId, text: &str, op: &[u8; 32], ts_ms: u64) {
        let mine = *author == self.me;
        if self.json {
            self.emit(json!({
                "event": "message",
                "author": hex_id(author),
                "text": text,
                "op": hex::encode(op),
                "mine": mine,
                "ts": ts_ms,
            }));
        } else {
            let who = if mine {
                "me".to_string()
            } else {
                short_id(author)
            };
            println!("[{}] {who}: {text}", clock(ts_ms));
        }
    }

    pub fn sync_started(&self, peer: &NodeId, incoming_ops: u32, incoming_bytes: u32) {
        if self.json {
            self.emit(json!({
                "event": "sync_started",
                "peer": hex_id(peer),
                "incoming_ops": incoming_ops,
                "incoming_bytes": incoming_bytes,
            }));
        } else if self.verbose {
            println!(
                "sync started with {}: {incoming_ops} ops, {incoming_bytes} bytes incoming",
                short_id(peer)
            );
        }
    }

    pub fn sync_ended(&self, peer: &NodeId, received: u32, sent: u32, error: Option<&str>) {
        if self.json {
            self.emit(json!({
                "event": "sync_ended",
                "peer": hex_id(peer),
                "received_ops": received,
                "sent_ops": sent,
                "error": error,
            }));
        } else if self.verbose {
            match error {
                Some(e) => println!(
                    "sync ended with {}: {received} received, {sent} sent, error: {e}",
                    short_id(peer)
                ),
                None => println!(
                    "sync ended with {}: {received} received, {sent} sent",
                    short_id(peer)
                ),
            }
        }
    }

    pub fn info(&self, text: &str) {
        if self.json {
            self.emit(json!({"event": "info", "text": text}));
        } else {
            println!("{text}");
        }
    }

    pub fn verbose(&self, text: &str) {
        if self.verbose {
            self.info(text);
        }
    }

    pub fn error(&self, text: &str) {
        if self.json {
            self.emit(json!({"event": "error", "text": text}));
        } else {
            println!("error: {text}");
        }
    }

    #[allow(dead_code)]
    pub fn raw(&self, value: Value) {
        self.emit(value);
    }
}
