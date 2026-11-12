//! H5: isolation of the model input, on the mock brain. One host with its
//! DM and two channels it hosts, `#garden` and `#other`.
//!
//! A string said only in the DM never appears in a channel turn; a string
//! said only in `#garden` never appears in a DM turn or an `#other` turn; the
//! DM turn's tool list contains nothing that reads or posts to a channel.
//!
//! A2: a memory saved in the DM never appears in any channel turn's input;
//! a channel turn is offered no `memory.*` tool, and a scripted call to one
//! from a channel is refused by the gate.

use std::collections::BTreeSet;

use serde_json::{Value, json};

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn reply_to(p: &mut Proc, channel: &str, ask: &str) {
    let want = format!("echo: {ask}");
    p.wait_for(&format!("reply to {ask:?} in #{channel}"), |v| {
        is_event(v, "message")
            && v["bot"] == true
            && v["channel"] == channel
            && v["text"].as_str() == Some(want.as_str())
    });
}

#[test]
fn turns_see_only_their_own_channel() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mock_log = work.path().join("mock.log");
    let script = work.path().join("script");
    std::fs::write(
        &script,
        "remember the okapi => call memory.save {\"text\": \"memory-only-okapi\", \"pinned\": true} => {result}\n\
         @bot garden recall => call memory.search {\"query\": \"okapi\"} => {result}\n",
    )
    .unwrap();
    let fake = work.path().join("fake.json");
    std::fs::write(
        &fake,
        json!({"log": work.path().join("calls.log"),
               "servers": {"calendar": ["create_event"], "linear": ["create_issue"]}})
        .to_string(),
    )
    .unwrap();
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_LOG", mock_log.to_str().unwrap()),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ("TRIAD_AGENT_TOOLS", "fake"),
        ("TRIAD_AGENT_FAKE_TOOLS", fake.to_str().unwrap()),
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
    a.send("/create other");
    a.wait_for("bot on in other", |v| {
        is_event(v, "bot_on") && v["channel"] == "other"
    });

    a.send("/switch agent");
    a.wait_for("switched", |v| is_event(v, "switched"));
    a.send("dm-only-zebra");
    reply_to(&mut a, "agent", "dm-only-zebra");
    a.send("remember the okapi");
    a.wait_for("saved", |v| {
        is_event(v, "message")
            && v["bot"] == true
            && v["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("Saved as memory"))
    });

    a.send("/msg garden garden-only-walrus");
    a.send("/msg garden @bot garden ask");
    reply_to(&mut a, "garden", "@bot garden ask");

    a.send("/msg other @bot other ask");
    reply_to(&mut a, "other", "@bot other ask");

    // The host asks the channel's bot to search memory: refused by the
    // gate, so the reply carries no memory.
    a.send("/msg garden @bot garden recall");
    let recall = a.wait_for("recall refused", |v| {
        is_event(v, "message")
            && v["bot"] == true
            && v["channel"] == "garden"
            && v["text"]
                .as_str()
                .is_some_and(|t| t.contains("memory.search"))
    });
    let recall_text = recall["text"].as_str().unwrap();
    assert!(
        recall_text.contains("only available in the personal DM"),
        "{recall_text}"
    );
    assert!(!recall_text.contains("okapi"), "{recall_text}");

    a.send("dm second ask");
    reply_to(&mut a, "agent", "dm second ask");
    a.quit();

    let turns: Vec<Value> = std::fs::read_to_string(&mock_log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(turns.len(), 6, "{turns:#?}");
    let find = |ask: &str| -> (Value, String) {
        let t = turns.iter().find(|t| t["ask"] == ask).unwrap().clone();
        let text = t.to_string();
        (t, text)
    };
    let (garden, garden_text) = find("@bot garden ask");
    let (other, other_text) = find("@bot other ask");
    let (dm2, dm2_text) = find("dm second ask");

    assert_eq!(garden["kind"], "channel");
    assert_eq!(garden["channel"], "garden");
    assert!(
        garden_text.contains("garden-only-walrus"),
        "control: garden sees garden"
    );
    assert!(!garden_text.contains("zebra"), "DM text in a channel turn");

    assert_eq!(other["channel"], "other");
    assert!(!other_text.contains("zebra"), "DM text in #other's turn");
    assert!(
        !other_text.contains("walrus"),
        "#garden text in #other's turn"
    );

    assert_eq!(dm2["kind"], "dm");
    assert!(
        dm2_text.contains("dm-only-zebra"),
        "control: the DM sees the DM"
    );
    assert!(!dm2_text.contains("walrus"), "#garden text in a DM turn");

    // The DM turn's tools are the host's MCP tools and the DM's own memory:
    // nothing that reads or posts to a channel exists to offer.
    let names = |t: &Value| -> BTreeSet<String> {
        t["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(&dm2),
        BTreeSet::from(
            [
                "calendar.create_event",
                "linear.create_issue",
                "memory.forget",
                "memory.save",
                "memory.search"
            ]
            .map(String::from)
        )
    );
    // The pinned memory reaches the DM's next turn...
    assert!(
        dm2_text.contains("memory-only-okapi"),
        "control: DM sees memory"
    );
    // ...and no channel turn, whose tools include no memory tool.
    for t in turns.iter().filter(|t| t["kind"] == "channel") {
        let text = t.to_string();
        assert!(!text.contains("okapi"), "memory in a channel turn: {text}");
        assert!(
            !text.contains("memory"),
            "memory named in a channel turn: {text}"
        );
        assert!(
            names(t).iter().all(|n| !n.starts_with("memory.")),
            "a channel turn was offered a memory tool"
        );
    }
    let _ = BIN;
}
