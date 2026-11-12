//! `triad bot …` and `/prompt`: seeing and changing what the personal bot
//! keeps. The editor is a stand-in that copies a prepared file over the one
//! it is given. Edits made while a chat session runs reach its next turn.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use triad_core::state::{State, now_ms};

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

/// Runs `triad <args>` with `EDITOR` copying `edited` over the file.
fn edit(dir: &Path, work: &Path, args: &[&str], edited: &str) -> (bool, String) {
    let file = work.join("edited");
    std::fs::write(&file, edited).unwrap();
    let out = Command::new(BIN)
        .env_remove("VISUAL")
        .env("EDITOR", format!("cp {}", file.display()))
        .arg("--data-dir")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn bot_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

#[test]
fn memories_schedules_skills_and_persona_can_be_seen_and_changed() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    {
        let state = State::open(d).unwrap();
        state.memory_save("The plumber is Dana", true).unwrap();
        state.memory_save("Dentist on Friday", false).unwrap();
        state
            .schedule_add("old", "Old.", now_ms() + 3_600_000, None)
            .unwrap();
    }
    let (code, out) = run_cmd(d, &["bot"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("memories:  2 (1 pinned)"), "{out}");
    assert!(out.contains("schedules: 1"), "{out}");

    // Memories: edit one, forget one, add one.
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "memory", "--edit"],
        "1 * The plumber is Lee\n+ Tea, no sugar\n",
    );
    assert!(ok, "{out}");
    assert!(out.contains("1 added, 1 changed, 1 forgotten"), "{out}");
    let (_, out) = run_cmd(d, &["bot", "memory"]);
    assert!(
        out.contains("#1 (") && out.contains("pinned) The plumber is Lee"),
        "{out}"
    );
    assert!(
        out.contains("Tea, no sugar") && !out.contains("Dentist"),
        "{out}"
    );
    // A bad edit changes nothing and keeps the file.
    let (ok, out) = edit(d, work.path(), &["bot", "memory", "--edit"], "9 nope\n");
    assert!(!ok);
    assert!(
        out.contains("no memory 9") && out.contains("Nothing changed"),
        "{out}"
    );
    assert!(d.join(".memory-edit.txt").exists());
    let (_, after) = run_cmd(d, &["bot", "memory"]);
    assert!(after.contains("The plumber is Lee"), "{after}");

    // Schedules: remove the old one, add a daily one.
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "schedules", "--edit"],
        "+ | in 1h | 1d | tea | Make tea.\n",
    );
    assert!(ok, "{out}");
    assert!(out.contains("1 added, 0 changed, 1 removed"), "{out}");
    let (_, out) = run_cmd(d, &["bot", "schedules"]);
    assert!(
        out.contains("tea: next ") && out.contains("every 1d"),
        "{out}"
    );
    assert!(!out.contains("old"), "{out}");

    // A skill and the persona, written in the editor.
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "skills", "standup", "--edit"],
        "---\ndescription: Run the standup.\n---\nAsk three things.\n",
    );
    assert!(ok, "{out}");
    let (_, out) = run_cmd(d, &["bot", "skills"]);
    assert_eq!(out, "standup: Run the standup.\n");
    let (code, _) = run_cmd(d, &["bot", "skills", "../x", "--edit"]);
    assert_ne!(code, 0);
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "persona", "--edit"],
        "PERSONA-ONE\n",
    );
    assert!(ok, "{out}");
    assert!(d.join("persona.md").exists());

    // A chat session sees them, and edits made while it runs reach the
    // next turn and /prompt.
    let log = work.path().join("mock.log");
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_LOG", log.to_str().unwrap()),
        ("TRIAD_AGENT_BUILTIN", "off"),
    ];
    let mut a = Proc::spawn_env("A", d, &["chat", "--json"], &env);
    a.wait_for("ready", |v| is_event(v, "ready"));
    a.send("first");
    a.wait_for("reply", |v| bot_text(v).as_deref() == Some("echo: first"));
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "persona", "--edit"],
        "PERSONA-TWO\n",
    );
    assert!(ok, "{out}");
    let (ok, out) = edit(
        d,
        work.path(),
        &["bot", "skills", "packing", "--edit"],
        "# Packing list\n- socks\n",
    );
    assert!(ok, "{out}");
    a.send("second");
    a.wait_for("reply", |v| bot_text(v).as_deref() == Some("echo: second"));
    a.send("/prompt");
    let prompt = a.wait_for("prompt", |v| is_event(v, "prompt"));
    a.quit();

    let turns: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let first = turns[0]["system"].as_str().unwrap();
    let second = turns[1]["system"].as_str().unwrap();
    assert!(
        first.contains("PERSONA-ONE") && first.contains("- The plumber is Lee"),
        "{first}"
    );
    assert!(first.contains("- standup: Run the standup.") && !first.contains("packing"));
    assert!(
        second.contains("PERSONA-TWO") && !second.contains("PERSONA-ONE"),
        "{second}"
    );
    assert!(second.contains("- packing: Packing list"), "{second}");
    let shown = prompt["system"].as_str().unwrap();
    assert!(
        shown.contains("PERSONA-TWO") && shown.contains("The plumber is Lee"),
        "{shown}"
    );
    let tools: Vec<&str> = prompt["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"memory.save") && tools.contains(&"skills.read"),
        "{tools:?}"
    );
}
