//! The session's side of the socket: one request at a time, inside the
//! event loop. Everything emitted while a request runs is tagged with it
//! (the printer's [`super::Current`]), and every request ends with exactly
//! one `done`.

use anyhow::{Context, Result, bail};
use serde_json::json;
use triad_core::identity::{hex_id, short_id};

use super::proto::{self, ClientId, Op, Request, Target};
use crate::{Session, Streams, parse_node};

/// `history` without a `limit`, and the most one page holds.
const HISTORY_DEFAULT: u64 = 50;
const HISTORY_MAX: u64 = 500;

impl Session {
    /// A client authenticated: send it `ready`.
    pub(crate) fn on_ws_connected(&mut self, client: ClientId) {
        self.printer.request.begin(Target { client, re: None });
        if let Err(e) = self.ready() {
            self.printer.error(&format!("{e:#}"));
        }
        self.printer.request.end();
    }

    pub(crate) async fn on_ws_request(&mut self, request: Request, streams: &mut Streams) {
        let Request { client, id, op } = request;
        self.printer.request.begin(Target {
            client,
            re: Some(id),
        });
        if let Err(e) = self.run_op(op, streams).await {
            self.printer.error(&format!("{e:#}"));
        }
        let error = self.printer.request.take_error();
        self.printer
            .emit(proto::done(error.as_deref()), String::new);
        self.printer.request.end();
    }

    async fn run_op(&mut self, op: Op, streams: &mut Streams) -> Result<()> {
        match op {
            Op::Send {
                channel,
                text,
                action,
            } => {
                let topic = self.resolve(&channel)?;
                if text.trim().is_empty() {
                    bail!("nothing to send");
                }
                self.send_body(topic, &text, action).await
            }
            Op::Command { channel, line } => {
                let line = line.trim();
                if !line.starts_with('/') {
                    bail!("a command starts with /; send messages with the send op");
                }
                let cmd = line.split_whitespace().next().unwrap_or(line);
                if proto::refused(cmd) {
                    bail!("{cmd} is not available over the socket");
                }
                let saved = self.focus;
                if let Some(channel) = channel {
                    self.focus = self.resolve(&channel)?;
                }
                let result = self.on_line(line, streams).await;
                // A client keeps its own focus; `/create` and `/join` move
                // the session's, which is put back.
                self.focus = saved;
                result.map(|_| ())
            }
            Op::Answer {
                channel,
                member,
                accept,
            } => {
                let topic = self.resolve(&channel)?;
                let member = parse_node(&member)?;
                let Some(at) = self.asks.iter().position(|a| *a == (topic, member)) else {
                    bail!(
                        "no join request from {} is waiting in #{}",
                        short_id(&member),
                        self.name(&topic)
                    );
                };
                self.asks.remove(at);
                if accept {
                    self.add(topic, member).await
                } else {
                    let name = self.name(&topic).to_string();
                    self.printer.emit(
                        json!({"event": "join_declined", "channel": name,
                               "member": hex_id(&member), "reason": "declined"}),
                        || format!("declined {} for #{name}", short_id(&member)),
                    );
                    Ok(())
                }
            }
            Op::History {
                channel,
                limit,
                before,
            } => self.history_page(&channel, limit, before.as_deref()),
            Op::Snapshot => self.ready(),
        }
    }

    /// Stored messages, newest last, each in the shape of a `message`
    /// event without `event` and `channel`.
    fn history_page(&self, channel: &str, limit: Option<u64>, before: Option<&str>) -> Result<()> {
        let topic = self.resolve(channel)?;
        let name = self.name(&topic).to_string();
        let limit = limit.unwrap_or(HISTORY_DEFAULT).clamp(1, HISTORY_MAX) as usize;
        let before = before
            .map(|h| -> Result<[u8; 32]> {
                hex::decode(h)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .context("\"before\" must be a message's op (64 hex characters)")
            })
            .transpose()?;
        let (page, more) = self
            .state
            .history_page(&topic, limit, before.as_ref())?
            .with_context(|| {
                format!(
                    "no message {} in #{name}",
                    before.map(hex::encode).unwrap_or_default()
                )
            })?;
        let profiles = self.state.profiles(&topic)?;
        let messages: Vec<serde_json::Value> = page
            .into_iter()
            .map(|m| {
                let nickname = profiles
                    .get(&m.author)
                    .cloned()
                    .or(m.nickname)
                    .filter(|n| !n.is_empty());
                let accepted = m.bot && self.bot_accepted(&topic, &m.author);
                let mut v = json!({"author": hex_id(&m.author), "text": m.text,
                    "nickname": nickname, "op": hex::encode(m.op_hash),
                    "mine": m.author == self.me,
                    "ts": m.op_ts.map_or(m.sent_at_ms, |t| t / 1000),
                    "bot": accepted, "action": m.action});
                if m.bot && !accepted {
                    v["bot_claim"] = json!("not accepted");
                }
                v
            })
            .collect();
        self.printer.emit(
            json!({"event": "history", "channel": name, "messages": messages, "more": more}),
            String::new,
        );
        Ok(())
    }
}
