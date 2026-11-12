//! A4: skills, on the mock brain. A scripted `skills.read` returns the
//! file's text; a name with `/` or `..` is refused. The DM turn's system
//! prompt lists the skill with its summary, after which the persona still
//! comes last.

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn bot_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

#[test]
fn skills_are_listed_and_read_inside_their_folder() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let skills = dir.path().join("skills");
    std::fs::create_dir(&skills).unwrap();
    std::fs::write(
        skills.join("standup.md"),
        "---\nname: standup\ndescription: Run the daily standup.\n---\nAsk what was done, \
         what is next, and what blocks it.\n",
    )
    .unwrap();
    // Outside the skills folder, in the data directory.
    std::fs::write(dir.path().join("private.md"), "private-outside-text").unwrap();
    let persona = work.path().join("persona.md");
    std::fs::write(&persona, "PERSONA-MARKER").unwrap();
    let script = work.path().join("script");
    std::fs::write(
        &script,
        "standup please => call skills.read {\"name\": \"standup\"} => {result}\n\
         dots please => call skills.read {\"name\": \"../private\"} => {result}\n\
         slash please => call skills.read {\"name\": \"x/../../private\"} => {result}\n",
    )
    .unwrap();
    let log = work.path().join("mock.log");
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ("TRIAD_AGENT_MOCK_LOG", log.to_str().unwrap()),
        ("TRIAD_AGENT_PERSONA", persona.to_str().unwrap()),
        ("TRIAD_AGENT_BUILTIN", "off"),
    ];
    let mut a = Proc::spawn_env("A", dir.path(), &["chat", "--json"], &env);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "agent");

    a.send("standup please");
    let v = a.wait_for("skill", |v| bot_text(v).is_some());
    let text = bot_text(&v).unwrap();
    assert!(
        text.starts_with("Skill standup, written by the host:"),
        "{text}"
    );
    assert!(text.contains("what blocks it."), "{text}");

    for ask in ["dots please", "slash please"] {
        a.send(ask);
        let v = a.wait_for("refused", |v| bot_text(v).is_some());
        let text = bot_text(&v).unwrap();
        assert!(text.contains("is not a skill name"), "{text}");
        assert!(!text.contains("private-outside-text"), "{text}");
    }
    a.quit();

    let first: Value = serde_json::from_str(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let system = first["system"].as_str().unwrap();
    assert!(
        system.contains("- standup: Run the daily standup.\n"),
        "{system}"
    );
    assert!(system.trim_end().ends_with("PERSONA-MARKER"), "{system}");
    let _ = BIN;
}
