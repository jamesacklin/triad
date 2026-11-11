//! H2: the personal bot in its one-member DM, on the mock brain.
//!
//! Three lines get three replies, in order, once each. A process killed with
//! SIGKILL after a job starts never sends that reply after restart, and does
//! not retry the job. Another node holding an invite to the DM's space ID is
//! never admitted. No DM text reaches `p2panda.sqlite`.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use triad_core::envelope::Invite;
use triad_core::net::DEFAULT_RELAY;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

const MOCK: (&str, &str) = ("TRIAD_AGENT_BRAIN", "mock");

fn bot_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

fn start(name: &'static str, dir: &Path, env: &[(&str, &str)]) -> (Proc, Value) {
    let mut p = Proc::spawn_env(name, dir, &["chat", "--agent", "--json"], env);
    let ready = p.wait_for("ready", |v| is_event(v, "ready"));
    (p, ready)
}

/// After a restart, a channel's space opens shortly after `ready`; lines
/// typed before that are refused, as for any channel.
fn restart(name: &'static str, dir: &Path, env: &[(&str, &str)]) -> Proc {
    let (mut p, ready) = start(name, dir, env);
    let me = ready["node"].as_str().unwrap().to_string();
    p.wait_for("space open", |v| {
        is_event(v, "joined") && v["member"] == me.as_str()
    });
    p
}

#[test]
fn personal_bot_in_a_one_member_dm() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // A fresh data directory: chat --agent creates the DM and focuses it.
    let (mut a, ready) = start("A", dir_a.path(), &[MOCK]);
    assert_eq!(ready["channel"], "agent");
    assert!(ready["invite"].is_null(), "the DM has no invite");
    let node_a = ready["node"].as_str().unwrap().to_string();
    let dm_topic = ready["group"].as_str().unwrap().to_string();

    // Three lines, three replies, in order, once each.
    let lines = ["dm-secret-one", "dm-secret-two", "dm-secret-three"];
    for l in lines {
        a.send(l);
    }
    let mut replies = Vec::new();
    while replies.len() < 3 {
        let v = a.wait_for("bot reply", |v| bot_text(v).is_some());
        assert_eq!(v["author"], node_a.as_str(), "a bot message is its owner's");
        replies.push(bot_text(&v).unwrap());
    }
    let want: Vec<String> = lines.iter().map(|l| format!("echo: {l}")).collect();
    assert_eq!(replies, want);
    let extra: Vec<String> = a
        .drain(Duration::from_secs(3))
        .iter()
        .filter_map(bot_text)
        .collect();
    assert!(extra.is_empty(), "no extra replies: {extra:?}");
    a.quit();

    // Kill -9 after a job starts: the reply is lost, never sent twice, and
    // the job is not retried.
    let mut a = restart(
        "A",
        dir_a.path(),
        &[MOCK, ("TRIAD_AGENT_MOCK_DELAY_MS", "8000")],
    );
    a.send("dm-interrupted");
    let started = a.wait_for("job started", |v| {
        is_event(v, "job") && v["state"] == "started"
    });
    let op = started["op"].as_str().unwrap().to_string();
    a.kill9();

    let mut a = restart("A", dir_a.path(), &[MOCK]);
    // The abandoned event is printed before ready.
    let abandoned = a
        .seen
        .iter()
        .find(|v| is_event(v, "job") && v["state"] == "abandoned")
        .cloned()
        .expect("the interrupted job is reported abandoned");
    assert_eq!(abandoned["op"], op.as_str());
    a.send("dm-after-restart");
    a.wait_for("reply after restart", |v| {
        bot_text(v).as_deref() == Some("echo: dm-after-restart")
    });
    let later = a.drain(Duration::from_secs(5));
    let all: Vec<&Value> = a.seen.iter().chain(later.iter()).collect();
    assert!(
        !all.iter()
            .any(|v| bot_text(v).as_deref() == Some("echo: dm-interrupted")),
        "the interrupted job was retried"
    );
    assert!(
        !all.iter()
            .any(|v| is_event(v, "job") && v["op"] == op.as_str() && v["state"] == "started"),
        "the abandoned job was started again"
    );

    // Another node with an invite to the DM's space ID is never admitted.
    let topic: [u8; 32] = hex::decode(&dm_topic).unwrap().try_into().unwrap();
    let via: [u8; 32] = hex::decode(&node_a).unwrap().try_into().unwrap();
    let invite = Invite::new(topic, via, DEFAULT_RELAY).render().unwrap();
    let mut b = Proc::spawn(
        "B",
        dir_b.path(),
        &["join", &invite, "--auto-accept", "--json"],
    );
    let node_b = b.wait_for("B ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    a.wait_for("join declined", |v| {
        is_event(v, "join_declined") && v["member"] == node_b.as_str()
    });
    let b_events = b.drain(Duration::from_secs(10));
    assert!(
        !b_events
            .iter()
            .any(|v| is_event(v, "joined") && v["member"] == node_b.as_str()),
        "B was admitted to the DM"
    );
    a.send("/members");
    let members = a.wait_for("members", |v| is_event(v, "members"));
    assert_eq!(members["members"].as_array().unwrap().len(), 1, "{members}");
    // /invite refuses the DM.
    a.send("/invite");
    a.wait_for("invite refused", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("bot DM")
    });
    b.quit();
    a.quit();

    // Each reply was sent once, and nothing of the DM is in p2panda's store.
    let (code, history) = run_cmd(dir_a.path(), &["history"]);
    assert_eq!(code, 0);
    eprintln!("A history:\n{history}");
    for l in lines.iter().chain(["dm-after-restart"].iter()) {
        let reply = format!("my bot: echo: {l}");
        assert_eq!(history.matches(&reply).count(), 1, "{reply} in\n{history}");
    }
    assert!(!history.contains("echo: dm-interrupted"));
    for needle in ["dm-secret-one", "dm-after-restart", "echo: dm-secret-two"] {
        let (code, out) = run_cmd(dir_a.path(), &["inspect", "--grep", needle]);
        assert_eq!(code, 0, "plaintext {needle} in p2panda.sqlite: {out}");
    }
    let (code, out) = run_cmd(dir_a.path(), &["invite", "--channel", "agent"]);
    assert_ne!(code, 0, "CLI invite to the DM: {out}");
}

#[test]
fn agent_without_a_key_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(BIN)
        .env_remove("ANTHROPIC_API_KEY")
        .env("TRIAD_AGENT_BRAIN", "anthropic")
        .arg("--data-dir")
        .arg(dir.path())
        .args(["chat", "--agent", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ANTHROPIC_API_KEY"));
}
