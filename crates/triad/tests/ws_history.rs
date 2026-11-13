//! W4: join requests and history over the socket. A `send` while a join
//! request waits is published and the request still waits; `answer`
//! declines and accepts; `history` pages through 120 messages with no
//! duplicate and no gap; a client that reconnects resyncs by `op`.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Value, json};

mod support;
use support::*;
#[path = "support/ws.rs"]
mod wsc;
use wsc::{Server, Ws, solo_channel, wait_open};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn history(ws: &mut Ws, req: Value) -> (Vec<Value>, bool) {
    let (done, replies) = ws.call(req);
    assert_eq!(done["ok"], true, "{done}");
    let page = replies
        .iter()
        .find(|v| v["event"] == "history")
        .expect("a history reply");
    (
        page["messages"].as_array().unwrap().clone(),
        page["more"] == true,
    )
}

fn own_messages(ws: &mut Ws, n: usize) {
    let count = |ws: &Ws| {
        ws.seen
            .iter()
            .filter(|v| v["event"] == "message" && v["mine"] == true)
            .count()
    };
    while count(ws) < n {
        ws.wait_for("own message", |v| {
            v["event"] == "message" && v["mine"] == true
        });
    }
}

#[test]
fn ws_join_requests_wait_for_an_answer() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir_a.path(), "garden");
    let server = Server::start(BIN, dir_a.path(), &[], &[]);
    let mut ws = server.connect("ws");
    let mut other = server.connect("other");
    let ready = ws.handshake();
    other.handshake();
    let me = ready["node"].as_str().unwrap().to_string();
    let invite = ready["invite"].as_str().unwrap().to_string();
    wait_open(&mut ws, "garden", &me);

    let mut b = Proc::spawn("B", dir_b.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    let ask = ws.wait_for("join request", |v| v["event"] == "join_request");
    assert_eq!(ask["member"], node_b.as_str());
    assert!(
        ask.get("re").is_none(),
        "a join request is news, not a reply"
    );
    other.wait_for("join request on the other client", |v| {
        v["event"] == "join_request" && v["member"] == node_b.as_str()
    });

    // "y" sent while the request waits is a message, not an answer.
    let (done, _) = ws.call(json!({"op": "send", "channel": "garden", "text": "y"}));
    assert_eq!(done["ok"], true);
    ws.wait_for("the y message", |v| {
        v["event"] == "message" && v["text"] == "y"
    });
    let waited = b.drain(Duration::from_secs(3));
    assert!(
        !waited
            .iter()
            .any(|v| is_event(v, "joined") && v["member"] == node_b.as_str()),
        "B was not let in by a chat message"
    );

    // Declined: everyone hears it, B stays out, and it cannot be answered
    // twice.
    let answer = json!({"op": "answer", "channel": "garden", "member": node_b, "accept": false});
    let (done, _) = ws.call(answer.clone());
    assert_eq!(done["ok"], true, "{done}");
    let declined = other.wait_for("declined", |v| v["event"] == "join_declined");
    assert_eq!(declined["member"], node_b.as_str());
    let (done, _) = ws.call(answer);
    assert_eq!(done["ok"], false, "{done}");
    assert!(
        !b.drain(Duration::from_secs(2))
            .iter()
            .any(|v| is_event(v, "joined") && v["member"] == node_b.as_str()),
        "B is still out"
    );
    drop(b);

    // Accepted: C is let in.
    let mut c = Proc::spawn("C", dir_c.path(), &["join", &invite, "--json"]);
    let node_c = c.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    ws.wait_for("C's join request", |v| {
        v["event"] == "join_request" && v["member"] == node_c.as_str()
    });
    let (done, _) =
        ws.call(json!({"op": "answer", "channel": "garden", "member": node_c, "accept": true}));
    assert_eq!(done["ok"], true, "{done}");
    c.wait_for("C joined", |v| {
        is_event(v, "joined") && v["member"] == node_c.as_str()
    });
    other.wait_for("C joined, on the socket", |v| {
        v["event"] == "joined" && v["member"] == node_c.as_str()
    });
    c.quit();
}

#[test]
fn ws_history_pages_and_resync() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);
    let mut one = server.connect("one");
    let ready = one.handshake();
    let me = ready["node"].as_str().unwrap().to_string();
    wait_open(&mut one, "garden", &me);

    let texts: Vec<String> = (0..120).map(|i| format!("m-{i:03}")).collect();
    for t in &texts {
        let (done, _) = one.call(json!({"op": "send", "channel": "garden", "text": t}));
        assert_eq!(done["ok"], true);
    }
    own_messages(&mut one, 120);

    // Newest first in pages, oldest first within a page.
    let mut pages = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut req = json!({"op": "history", "channel": "garden", "limit": 50});
        if let Some(b) = &before {
            req["before"] = json!(b);
        }
        let (page, more) = history(&mut one, req);
        assert!(!page.is_empty());
        before = Some(page[0]["op"].as_str().unwrap().to_string());
        pages.push(page);
        if !more {
            break;
        }
    }
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [50, 50, 20]);
    let all: Vec<Value> = pages.into_iter().rev().flatten().collect();
    let got: Vec<&str> = all.iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(got, texts, "no duplicate, no gap, oldest first");
    let first = &all[0];
    assert_eq!(first["mine"], true);
    assert_eq!(first["author"], me.as_str());
    // The same fields a live message has, less event and channel.
    let live = one
        .seen
        .iter()
        .find(|v| v["event"] == "message" && v["text"] == "m-000")
        .unwrap();
    assert_eq!(first["op"], live["op"]);
    assert_eq!(first["ts"], live["ts"]);

    // A cursor from another channel, or none at all, is an error.
    let (done, _) =
        one.call(json!({"op": "history", "channel": "garden", "before": "00".repeat(32)}));
    assert_eq!(done["ok"], false);

    // One goes away; meanwhile another client sends five more.
    let known: BTreeSet<String> = all
        .iter()
        .map(|m| m["op"].as_str().unwrap().into())
        .collect();
    drop(one);
    let mut two = server.connect("two");
    two.handshake();
    let late: Vec<String> = (0..5).map(|i| format!("late-{i}")).collect();
    for t in &late {
        let (done, _) = two.call(json!({"op": "send", "channel": "garden", "text": t}));
        assert_eq!(done["ok"], true);
    }
    own_messages(&mut two, 5);

    // One comes back: snapshot and the latest page, merged by op.
    let mut back = server.connect("back");
    back.handshake();
    let (done, replies) = back.call(json!({"op": "snapshot"}));
    assert_eq!(done["ok"], true);
    assert_eq!(replies[0]["event"], "ready");
    let (page, more) = history(
        &mut back,
        json!({"op": "history", "channel": "garden", "limit": 10}),
    );
    assert!(more);
    let missed: Vec<&str> = page
        .iter()
        .filter(|m| !known.contains(m["op"].as_str().unwrap()))
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(missed, late, "exactly the five missed, in order");
}
