//! Two `triad` processes, two data directories, one machine.
//!
//! Exercises create, join, 10 messages each way in order, kill -9 of the
//! joiner, 5 messages while it is down, and catch-up after restart.

use std::time::{Duration, Instant};

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

/// Whether message bodies are encrypted before p2panda sees them.
const ENCRYPTED: bool = true;

#[test]
fn two_node_chat_and_catch_up() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let started = Instant::now();

    let mut a = Proc::spawn("A", dir_a.path(), &["create", "--auto-accept", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();
    let node_a = ready["node"].as_str().unwrap().to_string();

    let mut b = Proc::spawn("B", dir_b.path(), &["join", &invite, "--json"]);
    let ready_b = b.wait_for("ready", |v| is_event(v, "ready"));
    let node_b = ready_b["node"].as_str().unwrap().to_string();
    wait_joined(&mut b, &ready_b);
    if ENCRYPTED {
        let j = a.wait_for("joined on A", |v| is_event(v, "joined"));
        assert_eq!(j["member"], node_b.as_str());
        assert_eq!(j["members"], 2);
    }
    eprintln!("joined after {:?}", started.elapsed());

    // 10 messages each way.
    let a_msgs: Vec<String> = (1..=10).map(|i| format!("mac-{i:02}")).collect();
    let b_msgs: Vec<String> = (1..=10).map(|i| format!("linux-{i:02}")).collect();
    for i in 0..10 {
        a.send(&a_msgs[i]);
        b.send(&b_msgs[i]);
    }
    assert_eq!(receive(&mut a, 10), b_msgs, "A got B's messages in order");
    assert_eq!(receive(&mut b, 10), a_msgs, "B got A's messages in order");
    eprintln!("20 messages exchanged after {:?}", started.elapsed());

    // B crashes. A sends 5 while B is down.
    b.kill9();
    let a_late: Vec<String> = (11..=15).map(|i| format!("mac-{i:02}")).collect();
    for m in &a_late {
        a.send(m);
        a.wait_for("own message", |v| {
            own_text(v).as_deref() == Some(m.as_str())
        });
    }

    let restart = Instant::now();
    let mut b = Proc::spawn("B", dir_b.path(), &["chat", "--json"]);
    b.wait_for("ready after restart", |v| is_event(v, "ready"));
    assert_eq!(
        receive(&mut b, 5),
        a_late,
        "B caught up on exactly the 5 missed"
    );
    eprintln!("catch-up of 5 took {:?}", restart.elapsed());
    let extra: Vec<String> = b
        .drain(Duration::from_secs(5))
        .iter()
        .filter_map(foreign_text)
        .collect();
    assert!(
        extra.is_empty(),
        "no duplicate deliveries after catch-up: {extra:?}"
    );

    b.quit();
    a.quit();

    // History: 25 unique messages on each side, each sender's messages in order.
    for (dir, name) in [(dir_a.path(), "A"), (dir_b.path(), "B")] {
        let (code, out) = run_cmd(dir, &["history"]);
        assert_eq!(code, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 25, "{name} history:\n{out}");
        let texts: Vec<&str> = lines
            .iter()
            .map(|l| l.rsplit(": ").next().unwrap())
            .collect();
        let mut unique = texts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 25, "{name} history has duplicates");
        let macs: Vec<&str> = texts
            .iter()
            .copied()
            .filter(|t| t.starts_with("mac-"))
            .collect();
        let mut sorted = macs.clone();
        sorted.sort();
        assert_eq!(macs, sorted, "{name}: A's messages in order");
    }

    // Plaintext inspection of what p2panda stored.
    for dir in [dir_a.path(), dir_b.path()] {
        for needle in ["mac-01", "linux-01", "mac-15"] {
            let (code, out) = run_cmd(dir, &["inspect", "--grep", needle]);
            let expected = if ENCRYPTED { 0 } else { 1 };
            assert_eq!(code, expected, "inspect {needle}: {out}");
        }
    }

    // Status: no failed operations.
    for dir in [dir_a.path(), dir_b.path()] {
        let (code, out) = run_cmd(dir, &["status"]);
        assert_eq!(code, 0);
        assert!(out.contains("failed=0"), "status:\n{out}");
        if ENCRYPTED {
            assert!(out.contains("epoch   1"), "status:\n{out}");
            assert!(
                out.contains(&node_a) && out.contains(&node_b),
                "status:\n{out}"
            );
        }
    }
}

/// After kill -9, does `StreamFrom::Frontier` with explicit acks replay exactly
/// the unacknowledged operations? B processes everything but acks nothing,
/// is killed, and restarts with acks on.
#[test]
fn unacked_operations_replay_after_kill() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let mut a = Proc::spawn("A", dir_a.path(), &["create", "--auto-accept", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();

    let mut b = Proc::spawn_env(
        "B",
        dir_b.path(),
        &["join", &invite, "--json", "-v"],
        &[("TRIAD_TEST_SKIP_ACK", "1")],
    );
    let ready_b = b.wait_for("ready", |v| is_event(v, "ready"));
    wait_joined(&mut b, &ready_b);
    let msgs = ["r-1", "r-2", "r-3"];
    for m in msgs {
        a.send(m);
    }
    assert_eq!(receive(&mut b, 3), msgs);
    b.kill9();

    // Restart with acks on. Stop A first so nothing new can arrive by sync.
    a.quit();
    let mut b = Proc::spawn("B", dir_b.path(), &["chat", "--json", "-v"]);
    let replay = b.wait_for("replay info", |v| {
        is_event(v, "info") && v["text"].as_str().unwrap_or("").starts_with("replaying")
    });
    eprintln!("REPLAY: {}", replay["text"]);
    let after = b.drain(Duration::from_secs(5));
    let dup: Vec<String> = after.iter().filter_map(foreign_text).collect();
    assert!(
        dup.is_empty(),
        "replayed operations must not be shown twice: {dup:?}"
    );
    b.quit();

    // Everything is acked now: a second restart replays nothing.
    let mut b = Proc::spawn("B", dir_b.path(), &["chat", "--json", "-v"]);
    let events = b.drain(Duration::from_secs(5));
    let replays: Vec<&Value> = events
        .iter()
        .filter(|v| {
            is_event(v, "info") && v["text"].as_str().unwrap_or("").starts_with("replaying")
        })
        .collect();
    eprintln!("SECOND RESTART replay events: {replays:?}");
    b.quit();

    let (_, out) = run_cmd(dir_b.path(), &["history"]);
    assert_eq!(out.lines().count(), 3, "history:\n{out}");
    let (_, status) = run_cmd(dir_b.path(), &["status"]);
    eprintln!("STATUS B:\n{status}");
    assert!(status.contains("failed=0"));
}

/// Milestone 7: a third member. Exercises `Envelope::Commit` and
/// `max_past_epochs`: B sends while A is committing C's add, and A still
/// decrypts it after moving to the next epoch.
#[test]
fn three_members_with_commit() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();

    // A answers join requests by hand so the test controls the commit timing.
    let mut a = Proc::spawn("A", dir_a.path(), &["create", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();

    let mut b = Proc::spawn("B", dir_b.path(), &["join", &invite, "--json"]);
    b.wait_for("ready", |v| is_event(v, "ready"));
    a.wait_for("B's join request", |v| is_event(v, "join_request"));
    a.send("y");
    let jb = b.wait_for("B joined", |v| is_event(v, "joined"));
    assert_eq!(
        (jb["epoch"].as_u64(), jb["members"].as_u64()),
        (Some(1), Some(2))
    );

    let mut c = Proc::spawn("C", dir_c.path(), &["join", &invite, "--json"]);
    let node_c = c.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    let req = a.wait_for("C's join request", |v| is_event(v, "join_request"));
    assert_eq!(req["member"], node_c.as_str());

    // B sends at epoch 1, then A commits C's add (epoch 2) right away.
    b.send("during-commit");
    b.wait_for("B's own message", |v| {
        own_text(v).as_deref() == Some("during-commit")
    });
    a.send("y");
    let ja = a.wait_for("C joined on A", |v| is_event(v, "joined"));
    assert_eq!(
        (ja["epoch"].as_u64(), ja["members"].as_u64()),
        (Some(2), Some(3))
    );
    let jc = c.wait_for("C joined", |v| is_event(v, "joined"));
    assert_eq!(
        (jc["epoch"].as_u64(), jc["members"].as_u64()),
        (Some(2), Some(3))
    );
    b.wait_for("B processed the commit", |v| {
        is_event(v, "info")
            && v["text"]
                .as_str()
                .unwrap_or("")
                .contains("epoch 2 with 3 members")
    });

    // A decrypts B's epoch-1 message even though A is at epoch 2.
    assert_eq!(receive(&mut a, 1), ["during-commit"]);

    // All three exchange messages at epoch 2.
    a.send("from-a");
    b.send("from-b");
    c.send("from-c");
    let mut got_a = receive(&mut a, 3);
    let mut got_b = receive(&mut b, 2);
    let mut got_c = receive(&mut c, 2);
    got_a.sort();
    got_b.sort();
    got_c.sort();
    assert_eq!(got_a, ["during-commit", "from-b", "from-c"]);
    assert_eq!(got_b, ["from-a", "from-c"]);
    assert_eq!(
        got_c,
        ["from-a", "from-b"],
        "C cannot read B's pre-join message"
    );

    c.quit();
    b.quit();
    a.quit();
    for (dir, name) in [
        (dir_a.path(), "A"),
        (dir_b.path(), "B"),
        (dir_c.path(), "C"),
    ] {
        let (_, status) = run_cmd(dir, &["status"]);
        eprintln!("STATUS {name}:\n{status}");
        assert!(status.contains("epoch   2"), "{name}:\n{status}");
        assert!(status.contains("members 3"), "{name}:\n{status}");
        assert!(status.contains("failed=0"), "{name}:\n{status}");
    }
}
