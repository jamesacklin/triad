//! A2: the personal bot's memory, on the mock brain.
//!
//! A scripted `memory.save` in the DM, then a restart, then a scripted
//! `memory.search` returns the text. The pinned memory appears in the next
//! DM turn's system prompt. `/memory` and `/forget` read and edit the same
//! rows.

use std::path::Path;

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

const FACT: &str = "The plumber is Dana, 555-0100";

fn bot_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// Starts chat mode. After a restart, waits until the DM's space is open
/// again; a fresh data directory creates it open.
fn start(dir: &Path, env: &[(&str, &str)], restart: bool) -> Proc {
    let mut p = Proc::spawn_env("A", dir, &["chat", "--json"], env);
    let ready = p.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "agent");
    let me = ready["node"].as_str().unwrap().to_string();
    if restart
        && !p
            .seen
            .iter()
            .any(|v| is_event(v, "joined") && v["member"] == me.as_str())
    {
        p.wait_for("space open", |v| {
            is_event(v, "joined") && v["member"] == me.as_str()
        });
    }
    p
}

fn reply(p: &mut Proc, what: &str) -> String {
    let v = p.wait_for(what, |v| bot_text(v).is_some());
    bot_text(&v).unwrap()
}

#[test]
fn memory_survives_a_restart_and_pins_reach_the_dm_prompt() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = work.path().join("script");
    std::fs::write(
        &script,
        format!(
            "remember the plumber => call memory.save {{\"text\": \"{FACT}\", \"pinned\": true}} => {{result}}\n\
             who is the plumber => call memory.search {{\"query\": \"plumber\"}} => {{result}}\n"
        ),
    )
    .unwrap();
    let log = work.path().join("mock.log");
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ("TRIAD_AGENT_MOCK_LOG", log.to_str().unwrap()),
        ("TRIAD_AGENT_BUILTIN", "off"),
    ];

    let mut a = start(dir.path(), &env, false);
    a.send("remember the plumber");
    assert_eq!(reply(&mut a, "save"), "Saved as memory #1, pinned.");
    a.quit();

    let mut a = start(dir.path(), &env, true);
    a.send("who is the plumber?");
    let found = reply(&mut a, "search");
    assert!(found.contains(FACT), "{found}");
    assert!(found.contains("#1 ("), "{found}");

    // The host's commands see the same row and can remove it.
    a.send("/memory plumber");
    let listed = a.wait_for("memory", |v| is_event(v, "memory"));
    assert_eq!(listed["memories"][0]["text"], FACT, "{listed}");
    assert_eq!(listed["memories"][0]["pinned"], true);
    a.send("/forget 1");
    let gone = a.wait_for("forgot", |v| is_event(v, "forgot"));
    assert_eq!(gone["removed"], true);
    a.send("/memory");
    let empty = a.wait_for("memory", |v| is_event(v, "memory"));
    assert_eq!(empty["memories"], serde_json::json!([]));
    a.send("after forgetting");
    reply(&mut a, "echo");
    a.quit();

    let turns: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(turns.len(), 3, "{turns:#?}");
    let system = |i: usize| turns[i]["system"].as_str().unwrap().to_string();
    // The turn that saved it had no note yet; the next DM turn has it, under
    // the heading; once forgotten it is gone.
    assert!(!system(0).contains(FACT));
    assert!(system(0).contains("save it with memory.save"));
    assert!(
        system(1).contains("The user's saved notes"),
        "{}",
        system(1)
    );
    assert!(system(1).contains(&format!("- {FACT}\n")), "{}", system(1));
    assert!(!system(2).contains(FACT));
    let names: Vec<&str> = turns[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["memory.forget", "memory.save", "memory.search"]);
    let _ = BIN;
}
