//! Several spaces per node: one identity and one data directory hold many
//! named channels, each with its own members and history.
//!
//! Three processes on one machine. A and B share `general`, `dev` and `ops`;
//! C is only in `dev`. Channels are created and joined both from the command
//! line and from inside chat mode, in both directions (A hosts two, B hosts
//! one), and B catches up on all of them after a restart.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn in_channel(v: &Value, event: &str, channel: &str) -> bool {
    is_event(v, event) && v["channel"] == channel
}

/// Sends a line and waits for it to come back as this node's own message in
/// `channel`.
fn say(p: &mut Proc, line: &str, channel: &str, text: &str) {
    p.send(line);
    p.wait_for("own message", |v| {
        in_channel(v, "message", channel) && own_text(v).as_deref() == Some(text)
    });
}

/// Waits for a foreign message with this text and returns its channel.
fn hear(p: &mut Proc, text: &str) -> String {
    let seen = p
        .seen
        .iter()
        .find(|v| foreign_text(v).as_deref() == Some(text))
        .cloned();
    let v = seen.unwrap_or_else(|| {
        p.wait_for(&format!("message {text}"), |v| {
            foreign_text(v).as_deref() == Some(text)
        })
    });
    v["channel"].as_str().unwrap().to_string()
}

/// Creates a channel from inside chat mode and returns its invite.
fn create_in_chat(p: &mut Proc, name: &str) -> String {
    p.send(&format!("/create {name}"));
    let v = p.wait_for("channel created", |v| in_channel(v, "channel", name));
    assert_eq!(v["role"], "creator");
    v["invite"].as_str().unwrap().to_string()
}

/// Waits until `p` reports `member` as joined in `channel`.
fn joined(p: &mut Proc, channel: &str, member: &str) {
    let already = p
        .seen
        .iter()
        .any(|v| in_channel(v, "joined", channel) && v["member"] == member);
    if !already {
        p.wait_for("joined", |v| {
            in_channel(v, "joined", channel) && v["member"] == member
        });
    }
}

fn history(dir: &Path, channel: Option<&str>) -> Vec<String> {
    let mut args = vec!["history"];
    if let Some(c) = channel {
        args.extend(["--channel", c]);
    }
    let (code, out) = run_cmd(dir, &args);
    assert_eq!(code, 0, "{out}");
    out.lines().map(str::to_string).collect()
}

#[test]
fn one_node_holds_many_channels() {
    let (da, db, dc) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );

    // A creates #general from the command line; B joins it.
    let mut a = Proc::spawn("A", da.path(), &["create", "--auto-accept", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "general");
    let node_a = ready["node"].as_str().unwrap().to_string();
    let general = ready["invite"].as_str().unwrap().to_string();

    let mut b = Proc::spawn(
        "B",
        db.path(),
        &["join", &general, "--auto-accept", "--json"],
    );
    let ready = b.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "general", "name comes from the invite");
    let node_b = ready["node"].as_str().unwrap().to_string();
    joined(&mut b, "general", &node_b);
    joined(&mut a, "general", &node_b);

    // A adds a second channel without leaving chat mode; B joins it the same way.
    let dev = create_in_chat(&mut a, "dev");
    b.send(&format!("/join {dev}"));
    b.wait_for("B sees #dev", |v| in_channel(v, "channel", "dev"));
    joined(&mut b, "dev", &node_b);
    joined(&mut a, "dev", &node_b);

    // The other direction: B hosts #ops and A joins it under its own name.
    let ops = create_in_chat(&mut b, "ops");
    a.send(&format!("/join {ops} b-ops"));
    a.wait_for("A sees #b-ops", |v| in_channel(v, "channel", "b-ops"));
    joined(&mut a, "b-ops", &node_a);
    joined(&mut b, "ops", &node_a);

    // C joins only #dev.
    let mut c = Proc::spawn("C", dc.path(), &["join", &dev, "--json"]);
    let node_c = c.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    joined(&mut c, "dev", &node_c);
    joined(&mut a, "dev", &node_c);
    joined(&mut b, "dev", &node_c);

    // Creating or joining focuses the new channel: A is in #b-ops, B in #ops.
    say(&mut a, "a-ops", "b-ops", "a-ops");
    assert_eq!(hear(&mut b, "a-ops"), "ops");
    // /msg sends elsewhere without switching.
    say(&mut a, "/msg #general a-general", "general", "a-general");
    assert_eq!(hear(&mut b, "a-general"), "general");
    // /switch moves the focus.
    a.send("/switch dev");
    a.wait_for("switched", |v| in_channel(v, "switched", "dev"));
    say(&mut a, "a-dev", "dev", "a-dev");
    assert_eq!(hear(&mut b, "a-dev"), "dev");
    assert_eq!(hear(&mut c, "a-dev"), "dev");
    b.send("/switch general");
    b.wait_for("switched", |v| in_channel(v, "switched", "general"));
    say(&mut b, "b-general", "general", "b-general");
    assert_eq!(hear(&mut a, "b-general"), "general");
    say(&mut c, "c-dev", "dev", "c-dev");
    assert_eq!(hear(&mut a, "c-dev"), "dev");
    assert_eq!(hear(&mut b, "c-dev"), "dev");

    // An unknown channel is an error, and the line is not sent anywhere.
    a.send("/switch nowhere");
    a.wait_for("error", |v| is_event(v, "error"));

    // /channels lists all three on A, with #dev focused.
    a.send("/channels");
    let list = a.wait_for("channels", |v| is_event(v, "channels"));
    let names: Vec<(&str, bool)> = list["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["channel"].as_str().unwrap(),
                c["focused"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(names, [("b-ops", false), ("dev", true), ("general", false)]);

    // B goes offline and misses one message in each of its three channels.
    b.kill9();
    say(&mut a, "late-dev", "dev", "late-dev");
    say(
        &mut a,
        "/msg general late-general",
        "general",
        "late-general",
    );
    say(&mut a, "/msg b-ops late-ops", "b-ops", "late-ops");

    let mut b = Proc::spawn("B", db.path(), &["chat", "--channel", "dev", "--json"]);
    let ready = b.wait_for("ready after restart", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "dev");
    assert_eq!(ready["channels"].as_array().unwrap().len(), 3);
    assert_eq!(hear(&mut b, "late-dev"), "dev");
    assert_eq!(hear(&mut b, "late-general"), "general");
    assert_eq!(hear(&mut b, "late-ops"), "ops");

    // C, in #dev only, was never shown anything from the other channels.
    let leaked: Vec<String> = c
        .drain(Duration::from_secs(5))
        .iter()
        .chain(c.seen.iter())
        .filter_map(foreign_text)
        .filter(|t| !t.ends_with("-dev"))
        .collect();
    assert!(leaked.is_empty(), "C saw other channels: {leaked:?}");

    c.quit();
    b.quit();
    a.quit();

    // Each channel keeps its own history.
    let texts = |dir: &Path, channel: &str| -> Vec<String> {
        let mut t: Vec<String> = history(dir, Some(channel))
            .iter()
            .map(|l| l.split_once(": ").unwrap().1.to_string())
            .collect();
        t.sort();
        t
    };
    for dir in [da.path(), db.path()] {
        assert_eq!(texts(dir, "dev"), ["a-dev", "c-dev", "late-dev"]);
        assert_eq!(
            texts(dir, "general"),
            ["a-general", "b-general", "late-general"]
        );
    }
    assert_eq!(texts(da.path(), "b-ops"), ["a-ops", "late-ops"]);
    assert_eq!(texts(db.path(), "ops"), ["a-ops", "late-ops"]);
    assert_eq!(texts(dc.path(), "dev"), ["a-dev", "c-dev", "late-dev"]);

    // Without --channel, one timeline with every line tagged.
    let all = history(da.path(), None);
    assert_eq!(all.len(), 8, "{all:?}");
    assert!(all.iter().all(|l| l.starts_with('#')), "{all:?}");
    assert_eq!(history(dc.path(), None).len(), 3);

    let (code, out) = run_cmd(db.path(), &["channels"]);
    assert_eq!(code, 0);
    let listed: Vec<&str> = out
        .lines()
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(listed, ["#general", "#dev", "#ops"]);
    assert!(out.lines().all(|l| l.contains(" joined ")), "{out}");

    for dir in [da.path(), db.path(), dc.path()] {
        let (_, status) = run_cmd(dir, &["status"]);
        assert!(
            status
                .lines()
                .filter(|l| l.starts_with("ops"))
                .all(|l| l.ends_with("failed=0")),
            "{status}"
        );
        for needle in ["a-general", "late-ops", "c-dev"] {
            let (code, out) = run_cmd(dir, &["inspect", "--grep", needle]);
            assert_eq!(code, 0, "plaintext {needle} in p2panda.sqlite: {out}");
        }
    }

    // A second `create` in the same data directory needs a new name.
    let (code, _) = run_cmd(da.path(), &["create", "--name", "general"]);
    assert_eq!(code, 1);
}
