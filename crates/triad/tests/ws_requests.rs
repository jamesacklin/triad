//! W3: requests. Two clients send to different channels at once and
//! neither moves the other; a query's reply reaches only its asker; a
//! failing command ends in `done` with `ok: false`; `/quit` and `/switch`
//! are refused. Also measures how long `/create` and `/join` hold the loop
//! as a second client sees it (FINDINGS.md).

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use triad_core::envelope::Invite;
use triad_core::net::DEFAULT_RELAY;

#[path = "support/ws.rs"]
mod wsc;
use wsc::{Server, Ws, solo_channel, wait_open};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn replies_named<'a>(replies: &'a [Value], event: &str) -> Vec<&'a Value> {
    replies.iter().filter(|v| v["event"] == event).collect()
}

/// How long `two` waits for a `snapshot` while `one` runs `line`.
fn stall(one: &mut Ws, two: &mut Ws, line: &str) -> (Duration, Value) {
    let id = one.request(json!({"op": "command", "line": line}));
    let started = Instant::now();
    let (done, _) = two.call(json!({"op": "snapshot"}));
    let waited = started.elapsed();
    assert_eq!(done["ok"], true);
    let (done, _) = one.wait_done(id);
    (waited, done)
}

#[test]
fn ws_requests_and_routing() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);
    let mut one = server.connect("one");
    let mut two = server.connect("two");
    let ready = one.handshake();
    two.handshake();
    let me = ready["node"].as_str().unwrap().to_string();
    assert_eq!(ready["channel"], "garden");
    wait_open(&mut one, "garden", &me);

    // A command that changes things: everyone hears of the new channel,
    // only the asker gets `re`.
    let (done, replies) = one.call(json!({"op": "command", "line": "/create pond --no-bot"}));
    assert_eq!(done["ok"], true, "{done}");
    let created = replies_named(&replies, "channel");
    assert_eq!(created.len(), 1, "{replies:?}");
    assert_eq!(created[0]["channel"], "pond");
    let theirs = two.wait_for("channel", |v| v["event"] == "channel");
    assert_eq!(theirs["channel"], "pond");
    assert!(theirs.get("re").is_none());
    wait_open(&mut one, "pond", &me);

    // Two clients, two channels, at the same time.
    let mut ids_one = Vec::new();
    let mut ids_two = Vec::new();
    for i in 0..10 {
        ids_one.push(
            one.request(json!({"op": "send", "channel": "garden", "text": format!("g-{i}")})),
        );
        ids_two
            .push(two.request(json!({"op": "send", "channel": "pond", "text": format!("p-{i}")})));
    }
    for id in ids_one {
        assert_eq!(one.wait_done(id).0["ok"], true);
    }
    for id in ids_two {
        assert_eq!(two.wait_done(id).0["ok"], true);
    }
    let count = |ws: &Ws| ws.seen.iter().filter(|v| v["event"] == "message").count();
    while count(&one) < 20 {
        one.wait_for("message", |v| v["event"] == "message");
    }
    for v in one.seen.iter().filter(|v| v["event"] == "message") {
        let text = v["text"].as_str().unwrap();
        let want = if text.starts_with("g-") {
            "garden"
        } else {
            "pond"
        };
        assert_eq!(v["channel"], want, "{text} landed in {}", v["channel"]);
    }

    // Neither moved the other: a command naming a channel acts there, one
    // naming none acts on the session's channel, which is still garden.
    let (_, replies) = two.call(json!({"op": "command", "channel": "pond", "line": "/members"}));
    assert_eq!(replies_named(&replies, "members")[0]["channel"], "pond");
    let (done, replies) = one.call(json!({"op": "command", "line": "/members"}));
    assert_eq!(done["ok"], true);
    let members = replies_named(&replies, "members");
    assert_eq!(members[0]["channel"], "garden");

    // The members reply went to `one` only: `two` asked about pond and
    // never sees garden's.
    two.call(json!({"op": "snapshot"}));
    let leaked: Vec<&Value> = two
        .seen
        .iter()
        .filter(|v| v["event"] == "members" && v["channel"] == "garden")
        .collect();
    assert!(leaked.is_empty(), "two saw one's reply: {leaked:?}");

    // A failing command: an error, then done with ok false.
    let (done, replies) = one.call(json!({"op": "command", "line": "/frobnicate"}));
    assert_eq!(done["ok"], false);
    assert!(
        done["error"].as_str().unwrap().contains("unknown command"),
        "{done}"
    );
    assert_eq!(replies_named(&replies, "error").len(), 1);
    let (done, _) = one.call(json!({"op": "command", "line": "/msg nowhere hi"}));
    assert_eq!(done["ok"], false);
    let (done, _) = one.call(json!({"op": "send", "channel": "nowhere", "text": "hi"}));
    assert_eq!(done["ok"], false);

    // Refused: the session's focus and life are not a client's to change.
    for line in ["/quit", "/switch pond", "/s pond", "/debug-bot-on"] {
        let (done, _) = one.call(json!({"op": "command", "line": line}));
        assert_eq!(done["ok"], false, "{line}");
        assert!(
            done["error"].as_str().unwrap().contains("not available"),
            "{done}"
        );
    }
    let (done, _) = one.call(json!({"op": "command", "line": "plain text"}));
    assert_eq!(done["ok"], false);
    let (done, _) = one.call(json!({"op": "command", "line": "/members"}));
    assert_eq!(done["ok"], true, "the session is still running");

    // Frames that are not requests.
    one.send_text("not json");
    let bad = one.wait_for("done", |v| v["event"] == "done");
    assert_eq!(bad["ok"], false);
    assert!(bad.get("re").is_none());
    one.send(&json!({"id": "x", "op": "send", "channel": "garden"}));
    let bad = one.wait_for("done", |v| v["event"] == "done");
    assert_eq!(
        (bad["ok"].clone(), bad["re"].clone()),
        (json!(false), json!("x"))
    );

    // How long the loop is held, as the other client sees it.
    let (created, done) = stall(&mut one, &mut two, "/create lagoon --no-bot");
    assert_eq!(done["ok"], true);
    let invite = Invite::new([7; 32], [9; 32], DEFAULT_RELAY)
        .with_name("faraway")
        .render()
        .unwrap();
    let (joined, done) = stall(&mut one, &mut two, &format!("/join {invite}"));
    assert_eq!(done["ok"], true, "{done}");
    let (quiet, _) = {
        let started = Instant::now();
        let r = two.call(json!({"op": "snapshot"}));
        (started.elapsed(), r)
    };
    eprintln!("STALL /create {created:?} /join {joined:?} idle {quiet:?}");
}
