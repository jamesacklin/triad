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
    let mut p = Proc::spawn_env(name, dir, &["chat", "--json"], env);
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

    // A fresh data directory: chat (bots on by default) creates the DM and
    // focuses it.
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

/// Bots are on by default. With no key for the model, a session says why
/// and runs as a plain chat; with no channel either, there is nothing to
/// open.
#[test]
fn no_model_key_runs_without_bots() {
    let dir = tempfile::tempdir().unwrap();
    let no_key = [
        ("TRIAD_NO_BOT", "0"),
        ("ANTHROPIC_API_KEY", ""),
        ("OPENAI_API_KEY", ""),
        ("TRIAD_AGENT_BRAIN", ""),
        ("TRIAD_AGENT_MODEL", ""),
    ];
    let out = std::process::Command::new(BIN)
        .envs(no_key)
        .arg("--data-dir")
        .arg(dir.path())
        .args(["chat", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");

    let mut a = Proc::spawn_env(
        "A",
        dir.path(),
        &["create", "--name", "garden", "--json"],
        &no_key,
    );
    let why = a.wait_for("bot_unavailable", |v| is_event(v, "bot_unavailable"));
    assert!(
        why["reason"]
            .as_str()
            .unwrap()
            .contains("ANTHROPIC_API_KEY"),
        "{why}"
    );
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "garden");
    assert_eq!(
        ready["channels"].as_array().unwrap().len(),
        1,
        "no DM: {ready}"
    );
    // /bot on retries, and says the same.
    a.send("/bot on");
    let e = a.wait_for("error", |v| is_event(v, "error"));
    assert!(
        e["text"].as_str().unwrap().contains("ANTHROPIC_API_KEY"),
        "{e}"
    );

    // /bot model saves the model; without its key the bots stay off and
    // the session says which key.
    a.send("/bot model openai/gpt-5.5");
    a.wait_for("model saved", |v| {
        is_event(v, "info") && v["text"].as_str().unwrap().contains("openai/gpt-5.5")
    });
    let off = a.wait_for("needs key", |v| {
        is_event(v, "info") && v["text"].as_str().unwrap().contains("bots not started")
    });
    assert!(
        off["text"].as_str().unwrap().contains("OPENAI_API_KEY"),
        "{off}"
    );
    a.send("/bot model");
    let m = a.wait_for("bot_model", |v| is_event(v, "bot_model"));
    assert_eq!(m["model"], "openai/gpt-5.5");
    a.send("/bot model nosuchvendor/x");
    let e = a.wait_for("bad vendor", |v| is_event(v, "error"));
    assert!(e["text"].as_str().unwrap().contains("nosuchvendor"), "{e}");

    // /bot provider-key saves a key for the model's vendor and starts the
    // bots, which create the DM.
    let key = "sk-test-provider-key-0123";
    a.send(&format!("/bot provider-key {key}"));
    let started = a.wait_for("bots_started", |v| is_event(v, "bots_started"));
    assert_eq!(started["provider"], "openai/gpt-5.5");
    a.wait_for("DM", |v| is_event(v, "channel") && v["channel"] == "agent");
    a.quit();

    let keys = dir.path().join(triad_agent::KEYS_FILE);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&keys).unwrap()).unwrap();
    assert_eq!(saved["openai"], key);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&keys).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let config = std::fs::read_to_string(dir.path().join("agent.json")).unwrap();
    assert!(
        config.contains("openai/gpt-5.5") && !config.contains(key),
        "{config}"
    );
    let log = std::fs::read_to_string(dir.path().join("triad.log")).unwrap_or_default();
    assert!(!log.contains(key), "key in triad.log");

    // The saved key lets the next session start its bots.
    let mut a = Proc::spawn_env("A", dir.path(), &["chat", "--json"], &no_key);
    let started = a.wait_for("bots_started", |v| {
        is_event(v, "ready") || is_event(v, "bot_unavailable")
    });
    assert!(is_event(&started, "ready"), "{started}");
    a.send("/bot");
    let b = a.wait_for("bot", |v| is_event(v, "bot"));
    assert_eq!(b["enabled"], true, "{b}");
    assert_eq!(b["provider"], "openai/gpt-5.5", "{b}");
    a.quit();
}

/// `triad reset` empties the data directory and makes a new identity. It
/// asks first (a pipe gets an error), refuses a directory that is not
/// triad's, and can keep the bot settings.
#[test]
fn reset_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let (_, old) = run_cmd(dir.path(), &["whoami"]);
    std::fs::write(
        dir.path().join("agent.json"),
        r#"{"model":"openai/gpt-5.5"}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("keys.json"), r#"{"openai":"k"}"#).unwrap();

    let (code, _) = run_cmd(dir.path(), &["reset"]);
    assert_ne!(code, 0, "reset without --yes from a pipe");
    assert_eq!(run_cmd(dir.path(), &["whoami"]).1, old);

    let (code, new) = run_cmd(dir.path(), &["reset", "--yes", "--keep-bot-config"]);
    assert_eq!(code, 0);
    assert_ne!(new, old);
    assert_eq!(run_cmd(dir.path(), &["whoami"]).1, new);
    assert!(dir.path().join("keys.json").exists());

    let (code, _) = run_cmd(dir.path(), &["reset", "--yes"]);
    assert_eq!(code, 0);
    assert!(!dir.path().join("keys.json").exists());
    assert!(!dir.path().join("agent.json").exists());

    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("notes.txt"), "mine").unwrap();
    let (code, _) = run_cmd(other.path(), &["reset", "--yes"]);
    assert_ne!(code, 0, "reset of a directory that is not triad's");
    assert!(other.path().join("notes.txt").exists());
}
