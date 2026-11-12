//! Long messages (`triad_core::long`): text over `SEND_TEXT_BYTES` travels
//! as ciphertext in an operation body on a plain p2panda stream, with its
//! key in a short space message.
//!
//! Host A runs the mock bot; B and C are members. A's long message reaches B
//! and C whole. B asks the bot with a long line; the bot's echo is long too,
//! and reaches B. A removes C, then sends another long message: B reads it,
//! C never does. Afterwards no node's `p2panda.sqlite` holds any of the
//! texts, and C's data directory holds nothing of the message sent after its
//! removal.
//!
//! A second test holds a long message's ciphertext back (a test hook), so
//! the reader gets the key first: nothing is shown until the body arrives,
//! also across a restart of the reader.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn long_text(marker: &str, n: usize) -> String {
    let mut s = format!("{marker} ");
    while s.len() < n {
        s.push_str("the quick brown fox jumps over the lazy dog ");
    }
    s.truncate(n);
    s
}

fn is_long_from(v: &Value, text: &str) -> bool {
    is_event(v, "message") && v["long"] == true && v["text"] == text
}

/// Occurrences of `needle` in every file of a data directory, or only in
/// files whose name starts with `prefix`.
fn found(dir: &Path, prefix: &str, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with(prefix) || !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
            hits.push(name);
        }
    }
    hits
}

#[test]
fn long_messages_between_members_and_not_to_a_removed_one() {
    let (da, db, dc) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &["create", "--name", "garden", "--auto-accept", "--json"],
        &[("TRIAD_AGENT_BRAIN", "mock")],
    );
    let invite = a.wait_for("ready", |v| is_event(v, "ready"))["invite"]
        .as_str()
        .unwrap()
        .to_string();
    let mut members = Vec::new();
    for (name, dir) in [("B", db.path()), ("C", dc.path())] {
        let mut p = Proc::spawn(name, dir, &["join", &invite, "--json"]);
        let id = p.wait_for("ready", |v| is_event(v, "ready"))["node"]
            .as_str()
            .unwrap()
            .to_string();
        p.wait_for("joined", |v| {
            is_event(v, "joined") && v["member"] == id.as_str()
        });
        p.wait_for("bot_on", |v| is_event(v, "bot_on"));
        members.push((p, id));
    }
    let (mut c, node_c) = members.pop().unwrap();
    let (mut b, _node_b) = members.pop().unwrap();

    // A long message from the host reaches both members whole.
    let one = long_text("LONGSECRET-ONE", 1500);
    a.send(&one);
    a.wait_for("own long message", |v| is_long_from(v, &one));
    b.wait_for("long message on B", |v| is_long_from(v, &one));
    c.wait_for("long message on C", |v| is_long_from(v, &one));

    // A long ask; the mock bot's echo of it is long too.
    let ask = format!("@bot {}", long_text("LONGASK", 900));
    b.send(&ask);
    let echo = format!("echo: {ask}");
    let reply = b.wait_for("long bot reply", |v| is_long_from(v, &echo));
    assert_eq!(reply["bot"], true);

    // C is removed; the next long message reaches B and never C.
    a.send(&format!("/remove {node_c}"));
    for p in [&mut a, &mut b] {
        p.wait_for("C removed", |v| {
            is_event(v, "removed") && v["member"] == node_c.as_str()
        });
    }
    let two = long_text("LONGSECRET-TWO", 1200);
    a.send(&two);
    b.wait_for("second long message on B", |v| is_long_from(v, &two));
    let late = c.drain(Duration::from_secs(8));
    assert!(
        !late.iter().any(|v| v["text"]
            .as_str()
            .is_some_and(|t| t.contains("LONGSECRET-TWO"))),
        "a removed member read a long message sent after its removal"
    );

    c.quit();
    b.quit();
    a.quit();

    for (who, dir) in [("A", da.path()), ("B", db.path()), ("C", dc.path())] {
        for marker in ["LONGSECRET-ONE", "LONGASK", "LONGSECRET-TWO"] {
            let hits = found(dir, "p2panda.sqlite", marker);
            assert!(hits.is_empty(), "{marker} in plaintext on {who}: {hits:?}");
            let (code, _) = run_cmd(dir, &["inspect", "--grep", marker]);
            assert_eq!(code, 0, "inspect found {marker} on {who}");
        }
    }
    let hits = found(dc.path(), "", "LONGSECRET-TWO");
    assert!(
        hits.is_empty(),
        "C's data directory holds the later message: {hits:?}"
    );
    // Control: the members' own state.sqlite holds what they read.
    assert!(!found(db.path(), "state.sqlite", "LONGSECRET-TWO").is_empty());
}

#[test]
fn the_key_can_arrive_before_the_body() {
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let hooks = [("TRIAD_AGENT_TEST_HOOKS", "1")];
    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &["create", "--auto-accept", "--json"],
        &hooks,
    );
    let invite = a.wait_for("ready", |v| is_event(v, "ready"))["invite"]
        .as_str()
        .unwrap()
        .to_string();
    let mut b = Proc::spawn("B", db.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    b.wait_for("joined", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    a.wait_for("B on A", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });

    let held = long_text("HELD-BODY", 800);
    a.send(&format!("/debug-long-hold {held}"));
    a.send("after-the-held-one");
    b.wait_for("the next message", |v| {
        foreign_text(v).as_deref() == Some("after-the-held-one")
    });
    // The space message arrived (it came before the next one) but its body
    // did not: nothing is shown, on B or on A.
    let shown = |p: &Proc| p.seen.iter().any(|v| is_long_from(v, &held));
    assert!(!shown(&b) && !shown(&a), "shown without its body");

    // B restarts while the message waits; the body arrives afterwards.
    b.quit();
    a.send("/debug-long-release");
    a.wait_for("own long message, once its body is in", |v| {
        is_long_from(v, &held)
    });
    let mut b = Proc::spawn("B", db.path(), &["chat", "--json"]);
    b.wait_for("long message on B after restart", |v| {
        is_long_from(v, &held)
    });
    b.drain(Duration::from_secs(2));
    assert_eq!(b.seen.iter().filter(|v| is_long_from(v, &held)).count(), 1);
    b.quit();
    a.quit();
    let _ = BIN;
}
