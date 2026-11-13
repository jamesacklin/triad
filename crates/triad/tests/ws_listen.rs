//! W1: `triad serve` broadcasts the session's events. Two nodes chat; a
//! socket client on one sees the same `message` events a `--json` pipe
//! does, numbered by `seq`, after `hello` and `ready`.

use std::collections::BTreeSet;

use serde_json::Value;

mod support;
use support::*;
#[path = "support/ws.rs"]
mod wsc;
use wsc::{Server, wait_open};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .unwrap()
        .keys()
        .filter(|k| !matches!(k.as_str(), "seq" | "re"))
        .cloned()
        .collect()
}

#[test]
fn ws_a_socket_client_sees_what_the_pipe_sees() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let mut a = Proc::spawn("A", dir_a.path(), &["create", "--auto-accept", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();

    // B joins over a pipe once, so its data directory has the channel.
    let mut b = Proc::spawn("B", dir_b.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    b.wait_for("B joined", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    b.quit();

    // Then B serves, with two socket clients.
    let server = Server::start(BIN, dir_b.path(), &[], &[]);
    let mut one = server.connect("ws1");
    let mut two = server.connect_frame("ws2");
    let hello = one.wait_for("hello", |v| v["event"] == "hello");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["node"], node_b.as_str());
    let ready_one = one.wait_for("ready", |v| v["event"] == "ready");
    assert_eq!(ready_one["node"], node_b.as_str());
    assert!(
        ready_one.get("re").is_none(),
        "ready on connect answers no request"
    );
    let channel = ready_one["channel"].as_str().unwrap().to_string();
    assert_eq!(two.handshake()["node"], node_b.as_str());
    wait_open(&mut one, &channel, &node_b);

    let texts = ["from-a-1", "from-a-2", "from-a-3"];
    for t in texts {
        a.send(t);
    }
    // The pipe's own echo of each message, for the shape.
    let piped: Vec<Value> = texts
        .iter()
        .map(|t| a.wait_for("own message", |v| own_text(v).as_deref() == Some(*t)))
        .collect();

    for ws in [&mut one, &mut two] {
        let mut got = Vec::new();
        let mut last_seq = 0;
        while got.len() < texts.len() {
            let v = ws.wait_for("message", |v| v["event"] == "message");
            let seq = v["seq"].as_u64().expect("every frame has a seq");
            assert!(seq > last_seq, "seq rises: {seq} after {last_seq}");
            last_seq = seq;
            assert_eq!(v["channel"], channel.as_str());
            assert_eq!(v["mine"], false);
            assert!(v.get("re").is_none(), "a broadcast carries no re: {v}");
            got.push(v);
        }
        let got_texts: Vec<&str> = got.iter().map(|v| v["text"].as_str().unwrap()).collect();
        assert_eq!(got_texts, texts, "{}: in order", ws.name);
        for (s, p) in got.iter().zip(&piped) {
            assert_eq!(
                keys(s),
                keys(p),
                "the socket's message has the pipe's fields"
            );
            assert_eq!(s["op"], p["op"]);
            assert_eq!(s["author"], p["author"]);
        }
    }
    // Both clients saw each message under the same seq.
    let seqs = |ws: &wsc::Ws| -> Vec<u64> {
        ws.seen
            .iter()
            .filter(|v| v["event"] == "message")
            .map(|v| v["seq"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(seqs(&one), seqs(&two));
    a.quit();
}
