//! W5: the personal bot over the socket, on the mock brain. Nothing is
//! socket-specific: a turn's `job`, `tool_call`, reply and a schedule
//! firing are ordinary events, broadcast to every client; the bot's query
//! commands answer only the asker.

use serde_json::{Value, json};

#[path = "support/ws.rs"]
mod wsc;
use wsc::{Server, Ws, wait_open};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

const SCRIPT: &str = "\
remind me => call schedule.add {\"name\": \"standup\", \"prompt\": \"Check the build.\", \"at\": \"in 2s\"} => {result}\n\
[scheduled: standup] => Time to check the build.\n";

fn bot_text(v: &Value) -> Option<&str> {
    (v["event"] == "message" && v["bot"] == true).then(|| v["text"].as_str().unwrap())
}

fn wait_bot(ws: &mut Ws, what: &str, pred: impl Fn(&str) -> bool) -> Value {
    ws.wait_for(what, |v| bot_text(v).is_some_and(&pred))
}

#[test]
fn ws_the_personal_bot_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("script"), SCRIPT).unwrap();
    let script = dir.path().join("script");
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
    ];
    // A fresh directory: with bots on, serve opens on the bot's DM.
    let server = Server::start(BIN, dir.path(), &[], &env);
    let mut one = server.connect("one");
    let mut two = server.connect("two");
    let ready = one.handshake();
    two.handshake();
    assert_eq!(ready["bots"], true);
    assert_eq!(ready["bot_provider"], "mock");
    let dm = ready["channel"].as_str().unwrap().to_string();
    let me = ready["node"].as_str().unwrap().to_string();
    wait_open(&mut one, &dm, &me);

    let (done, _) = one.call(json!({"op": "send", "channel": dm, "text": "remind me tomorrow"}));
    assert_eq!(done["ok"], true, "{done}");

    for ws in [&mut one, &mut two] {
        let reply = wait_bot(ws, "the bot's reply", |t| t.contains("standup"));
        assert_eq!(reply["channel"], dm.as_str());
        assert!(reply.get("re").is_none(), "a reply arrives later, as news");
        let states: Vec<&str> = ws
            .seen
            .iter()
            .filter(|v| v["event"] == "job")
            .map(|v| v["state"].as_str().unwrap())
            .collect();
        assert!(
            states.contains(&"queued") || states.contains(&"started"),
            "{states:?}"
        );
        assert!(
            ws.seen
                .iter()
                .any(|v| v["event"] == "tool_call" && v["tool"] == "schedule.add"),
            "{}: the tool call was shown",
            ws.name
        );
        ws.wait_for("fired", |v| v["event"] == "schedule_fired");
        wait_bot(ws, "the scheduled reply", |t| {
            t == "Time to check the build."
        });
    }

    // The bot's query commands answer the asker only.
    let (done, replies) = two.call(json!({"op": "command", "line": "/schedules"}));
    assert_eq!(done["ok"], true);
    assert!(replies.iter().any(|v| v["event"] == "schedules"));
    let (done, _) = one.call(json!({"op": "snapshot"}));
    assert_eq!(done["ok"], true);
    assert!(
        !one.seen.iter().any(|v| v["event"] == "schedules"),
        "one never asked for the schedules"
    );
}
