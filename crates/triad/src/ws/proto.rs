//! The socket's request vocabulary: parsing a client's frame into an
//! [`Op`], which events answer a request only, and which commands the
//! generic `command` operation refuses. API.md is the reference.

use serde_json::{Value, json};

/// The protocol version `hello` announces. Bumped only for a change a
/// client could not ignore; new events and fields do not bump it.
pub const PROTOCOL: u64 = 1;

/// One connection, numbered from 1 in the order they authenticated.
pub type ClientId = u64;

/// Who an event emitted while handling a request belongs to: the client,
/// and the request's `id` (none for the `ready` sent on connecting).
#[derive(Clone, Debug)]
pub struct Target {
    pub client: ClientId,
    pub re: Option<Value>,
}

/// A parsed request.
#[derive(Debug)]
pub enum Op {
    Send {
        channel: String,
        text: String,
        action: bool,
    },
    Answer {
        channel: String,
        member: String,
        accept: bool,
    },
    History {
        channel: String,
        limit: Option<u64>,
        before: Option<String>,
    },
    Snapshot,
    Command {
        channel: Option<String>,
        line: String,
    },
}

/// A request from an authenticated client, as the session receives it.
#[derive(Debug)]
pub struct Request {
    pub client: ClientId,
    pub id: Value,
    pub op: Op,
}

/// Why a frame was not a request, and the `id` to answer it under if it
/// had a usable one.
#[derive(Debug)]
pub struct Malformed {
    pub id: Option<Value>,
    pub error: String,
}

/// Events that answer the request they were emitted under and go to its
/// client only. Anything emitted under a request that is not in
/// [`BROADCAST`] is treated the same way, so a new query command's reply is
/// never broadcast by mistake.
#[cfg(test)]
pub const REPLY: &[&str] = &[
    "hello",
    "ready",
    "info",
    "error",
    "channels",
    "members",
    "invite",
    "grants",
    "audit",
    "memory",
    "prompt",
    "schedules",
    "history",
    "done",
    "bot",
    "bot_model",
];

/// Events that report a change every client should see, even when a
/// request caused them.
pub const BROADCAST: &[&str] = &[
    "message",
    "joined",
    "promoted",
    "removed",
    "channel",
    "nick",
    "granted",
    "revoked",
    "blocked",
    "unblocked",
    "bot_on",
    "bot_off",
    "bots_started",
    "bot_unavailable",
    "bot_rejected",
    "job",
    "tool_call",
    "schedule_fired",
    "schedule_missed",
    "unscheduled",
    "forgot",
    "join_request",
    "join_declined",
    "sync_started",
    "sync_ended",
];

/// Whether an event emitted under a request goes to every client.
pub fn broadcast(event: &str) -> bool {
    BROADCAST.contains(&event)
}

/// Commands `command` refuses: they act on the session as a whole
/// (`/quit`), on its focus, which a socket client keeps for itself
/// (`/switch`), or are test hooks.
pub fn refused(cmd: &str) -> bool {
    matches!(cmd, "/quit" | "/switch" | "/s") || cmd.starts_with("/debug-")
}

fn string(v: &Value, field: &str) -> Result<String, String> {
    match &v[field] {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Err(format!("missing \"{field}\"")),
        _ => Err(format!("\"{field}\" must be a string")),
    }
}

fn opt_string(v: &Value, field: &str) -> Result<Option<String>, String> {
    match &v[field] {
        Value::Null => Ok(None),
        _ => string(v, field).map(Some),
    }
}

fn opt_bool(v: &Value, field: &str) -> Result<Option<bool>, String> {
    match &v[field] {
        Value::Null => Ok(None),
        Value::Bool(b) => Ok(Some(*b)),
        _ => Err(format!("\"{field}\" must be true or false")),
    }
}

/// Reads one text frame from an authenticated client.
pub fn parse(text: &str) -> Result<(Value, Op), Malformed> {
    let v: Value = serde_json::from_str(text).map_err(|e| Malformed {
        id: None,
        error: format!("not JSON: {e}"),
    })?;
    if !v.is_object() {
        return Err(Malformed {
            id: None,
            error: "a request is a JSON object".into(),
        });
    }
    let id = match &v["id"] {
        id @ (Value::Number(_) | Value::String(_)) => id.clone(),
        Value::Null => {
            return Err(Malformed {
                id: None,
                error: "a request needs an \"id\" (a number or a string)".into(),
            });
        }
        _ => {
            return Err(Malformed {
                id: None,
                error: "\"id\" must be a number or a string".into(),
            });
        }
    };
    let fail = |error: String| Malformed {
        id: Some(id.clone()),
        error,
    };
    let op = string(&v, "op").map_err(fail)?;
    let op = match op.as_str() {
        "send" => Op::Send {
            channel: string(&v, "channel").map_err(fail)?,
            text: string(&v, "text").map_err(fail)?,
            action: opt_bool(&v, "action").map_err(fail)?.unwrap_or(false),
        },
        "answer" => Op::Answer {
            channel: string(&v, "channel").map_err(fail)?,
            member: string(&v, "member").map_err(fail)?,
            accept: opt_bool(&v, "accept")
                .map_err(fail)?
                .ok_or_else(|| fail("missing \"accept\"".into()))?,
        },
        "history" => Op::History {
            channel: string(&v, "channel").map_err(fail)?,
            limit: match &v["limit"] {
                Value::Null => None,
                l => Some(
                    l.as_u64()
                        .ok_or_else(|| fail("\"limit\" must be a positive integer".into()))?,
                ),
            },
            before: opt_string(&v, "before").map_err(fail)?,
        },
        "snapshot" => Op::Snapshot,
        "command" => Op::Command {
            channel: opt_string(&v, "channel").map_err(fail)?,
            line: string(&v, "line").map_err(fail)?,
        },
        "auth" => return Err(fail("already authenticated".into())),
        other => return Err(fail(format!("unknown op {other:?}"))),
    };
    Ok((id, op))
}

/// The token from a first-frame `auth` request, if that is what it is.
pub fn auth_token(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    (v["op"] == "auth").then(|| v["token"].as_str().map(str::to_owned))?
}

/// The `done` that ends a request.
pub fn done(error: Option<&str>) -> Value {
    match error {
        None => json!({"event": "done", "ok": true}),
        Some(e) => json!({"event": "done", "ok": false, "error": e}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_op() {
        let (id, op) = parse(r#"{"id":1,"op":"send","channel":"g","text":"hi"}"#).unwrap();
        assert_eq!(id, json!(1));
        assert!(matches!(op, Op::Send { action: false, .. }));
        let (id, op) = parse(r#"{"id":"a","op":"command","line":"/members"}"#).unwrap();
        assert_eq!(id, json!("a"));
        assert!(matches!(op, Op::Command { channel: None, .. }));
        assert!(matches!(
            parse(r#"{"id":2,"op":"history","channel":"g","limit":5}"#)
                .unwrap()
                .1,
            Op::History { limit: Some(5), .. }
        ));
        assert!(matches!(
            parse(r#"{"id":3,"op":"snapshot"}"#).unwrap().1,
            Op::Snapshot
        ));
    }

    #[test]
    fn rejects_with_the_id_when_there_is_one() {
        let e = parse(r#"{"id":7,"op":"send","channel":"g"}"#).unwrap_err();
        assert_eq!(e.id, Some(json!(7)));
        assert!(e.error.contains("text"), "{}", e.error);
        let e = parse(r#"{"op":"snapshot"}"#).unwrap_err();
        assert_eq!(e.id, None);
        assert!(parse("nope").is_err());
        assert!(parse(r#"{"id":1,"op":"auth","token":"x"}"#).is_err());
        assert!(parse(r#"{"id":1,"op":"frobnicate"}"#).is_err());
    }

    #[test]
    fn reply_and_broadcast_sets_do_not_overlap() {
        for name in REPLY {
            assert!(!broadcast(name), "{name} is in both sets");
        }
    }

    #[test]
    fn refuses_focus_and_session_commands() {
        for cmd in ["/quit", "/switch", "/s", "/debug-bot-on"] {
            assert!(refused(cmd), "{cmd}");
        }
        assert!(!refused("/members"));
    }

    #[test]
    fn auth_frame() {
        assert_eq!(
            auth_token(r#"{"op":"auth","token":"abc"}"#).as_deref(),
            Some("abc")
        );
        assert_eq!(auth_token(r#"{"op":"send"}"#), None);
        assert_eq!(auth_token("x"), None);
    }
}
