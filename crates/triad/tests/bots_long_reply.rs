//! A5: room to answer. A mock reply of 5,000 bytes arrives whole in
//! `#agent`, as a long message; the same reply in a channel is still cut to
//! one message's limit.

use serde_json::Value;
use triad_core::envelope::MAX_TEXT_BYTES;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn bot_reply(v: &Value, channel: &str) -> bool {
    is_event(v, "message") && v["bot"] == true && v["channel"] == channel
}

#[test]
fn dm_replies_are_long_and_channel_replies_are_cut() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 5,000 bytes: numbered chunks, an end marker, then padding.
    let mut reply: String = (0..).map(|i| format!("{i:04}-chunk ")).take(453).collect();
    reply.push_str("THE-END");
    reply = format!("{reply:x<5000}");
    assert_eq!(reply.len(), 5000);
    let script = work.path().join("script");
    std::fs::write(&script, format!("long please => {reply}\n")).unwrap();
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ("TRIAD_AGENT_BUILTIN", "off"),
    ];
    let mut a = Proc::spawn_env(
        "A",
        dir.path(),
        &["create", "--name", "garden", "--json"],
        &env,
    );
    a.wait_for("ready", |v| is_event(v, "ready"));
    a.wait_for("bot on in garden", |v| {
        is_event(v, "bot_on") && v["channel"] == "garden"
    });

    a.send("/switch agent");
    a.wait_for("switched", |v| is_event(v, "switched"));
    a.send("long please");
    let dm = a.wait_for("DM reply", |v| bot_reply(v, "agent"));
    assert_eq!(dm["long"], true, "{dm}");
    assert_eq!(dm["text"].as_str().unwrap(), reply, "the DM reply was cut");

    a.send("/msg garden @bot long please");
    let ch = a.wait_for("channel reply", |v| bot_reply(v, "garden"));
    let text = ch["text"].as_str().unwrap();
    assert!(text.len() <= MAX_TEXT_BYTES, "{}", text.len());
    assert!(text.ends_with('…'), "the channel reply was not cut");
    assert!(reply.starts_with(text.trim_end_matches('…')));
    a.quit();

    // The stored DM history keeps the whole reply.
    let (code, out) = run_cmd(dir.path(), &["history", "--channel", "agent"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("THE-END"), "{out}");
    let _ = BIN;
}
