//! Topics: `/topic <name>` opens a sub-channel under the focused channel,
//! a space of its own filed under its channel as `channel/name`.
//!
//! A hosts #general without --auto-accept and opens #general/ideas. B, a
//! member, joins the topic on reading the announcement and is let in
//! without a prompt. C joins #general later and finds the topic in the
//! history. Removed from #general, C is removed from the topic too. Only a
//! manager opens topics, and a topic has none. After a restart, the topic
//! is still filed under its channel.
//!
//! The channel's bot opens one with `channel.topic`, and joins it.

use std::time::Duration;

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn in_channel(v: &Value, event: &str, channel: &str) -> bool {
    is_event(v, event) && v["channel"] == channel
}

fn wait_in(p: &mut Proc, event: &str, channel: &str, member: &str) -> Value {
    if let Some(v) = p
        .seen
        .iter()
        .find(|v| in_channel(v, event, channel) && v["member"] == member)
    {
        return v.clone();
    }
    p.wait_for(&format!("{event} {member} in #{channel}"), |v| {
        in_channel(v, event, channel) && v["member"] == member
    })
}

fn join(name: &'static str, dir: &std::path::Path, invite: &str) -> (Proc, String) {
    let mut p = Proc::spawn(name, dir, &["join", invite, "--json"]);
    let id = p.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    (p, id)
}

fn error(p: &mut Proc) -> String {
    p.wait_for("error", |v| is_event(v, "error"))["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn topics_follow_their_channel() {
    let (da, db, dc) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let mut a = Proc::spawn("A", da.path(), &["create", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();
    assert!(ready["channels"][0]["parent"].is_null());

    let (mut b, node_b) = join("B", db.path(), &invite);
    a.wait_for("B's join request", |v| is_event(v, "join_request"));
    a.send("y");
    wait_in(&mut b, "joined", "general", &node_b);

    // A opens a topic; B joins it on its own, and A lets B in unasked.
    a.send("/topic Ideas");
    let v = a.wait_for("A's topic", |v| in_channel(v, "channel", "general/ideas"));
    assert_eq!(v["parent"], "general");
    assert_eq!(v["role"], "creator");
    let v = b.wait_for("B's topic", |v| in_channel(v, "channel", "general/ideas"));
    assert_eq!(v["parent"], "general");
    assert_eq!(v["role"], "member");
    let ask = wait_in(&mut a, "join_request", "general/ideas", &node_b);
    assert_eq!(ask["auto"], true);
    let v = wait_in(&mut b, "joined", "general/ideas", &node_b);
    assert_eq!(v["access"], "write", "B's access in #general");

    b.send("/msg general/ideas hello topic");
    let v = a.wait_for("B's message", |v| {
        foreign_text(v).as_deref() == Some("hello topic")
    });
    assert_eq!(v["channel"], "general/ideas");

    // Only a manager opens topics, and a topic has none.
    b.send("/topic nope");
    assert!(error(&mut b).contains("Manage access"));
    a.send("/topic deeper");
    assert!(error(&mut a).contains("is a topic"));
    a.send("/create x/y");
    assert!(error(&mut a).contains("/topic"));

    // C joins #general later and finds the topic in its history.
    let (mut c, node_c) = join("C", dc.path(), &invite);
    a.wait_for("C's join request", |v| {
        in_channel(v, "join_request", "general") && v["member"] == node_c.as_str()
    });
    a.send("y");
    let v = c.wait_for("C's topic", |v| in_channel(v, "channel", "general/ideas"));
    assert_eq!(v["parent"], "general");
    wait_in(&mut c, "joined", "general/ideas", &node_c);

    // Removed from #general, C leaves the topic too.
    a.send("/switch general");
    a.send(&format!("/remove {node_c}"));
    wait_in(&mut a, "removed", "general", &node_c);
    wait_in(&mut a, "removed", "general/ideas", &node_c);
    wait_in(&mut b, "removed", "general/ideas", &node_c);

    // The topic stays filed under its channel.
    a.quit();
    let mut a = Proc::spawn("A", da.path(), &["chat", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let topic = ready["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["channel"] == "general/ideas")
        .cloned()
        .expect("topic after restart");
    assert_eq!(topic["parent"], "general");
    a.quit();
    b.quit();
    c.quit();
}

#[test]
fn the_bot_opens_topics() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script");
    std::fs::write(
        &script,
        "a topic => call channel.topic {\"name\": \"plans\"} => Done: {result}\n",
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut a = Proc::spawn_env(
        "A",
        data.path(),
        &["create", "--name", "garden", "--json"],
        &[
            ("TRIAD_AGENT_BRAIN", "mock"),
            ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ],
    );
    let node_a = a.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    a.wait_for("bot on", |v| in_channel(v, "bot_on", "garden"));

    a.send("@bot open a topic for plans");
    let v = a.wait_for("the topic", |v| in_channel(v, "channel", "garden/plans"));
    assert_eq!(v["parent"], "garden");
    let reply = a.wait_for("the reply", |v| {
        in_channel(v, "message", "garden") && v["bot"] == true
    });
    assert!(
        reply["text"]
            .as_str()
            .unwrap()
            .contains("Opened #garden/plans"),
        "{reply}"
    );
    // The channel's bot is in the topic too.
    let on = match a
        .seen
        .iter()
        .find(|v| in_channel(v, "bot_on", "garden/plans"))
    {
        Some(v) => v.clone(),
        None => a.wait_for("bot on in the topic", |v| {
            in_channel(v, "bot_on", "garden/plans")
        }),
    };
    assert_eq!(on["host"], node_a.as_str());
    a.drain(Duration::from_millis(200));
    a.quit();
}
