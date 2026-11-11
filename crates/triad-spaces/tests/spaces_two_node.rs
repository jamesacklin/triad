//! Milestone 6: the same two-node test as `triad`'s, run against
//! `triad-spaces` (p2panda encrypted spaces, no OpenMLS).

use std::time::{Duration, Instant};

#[path = "../../triad-cli/tests/support/mod.rs"]
mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad-spaces");

#[test]
fn spaces_two_node_chat_and_catch_up() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let started = Instant::now();

    let mut a = Proc::spawn("A", dir_a.path(), &["create", "--auto-accept", "--json"]);
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();

    let mut b = Proc::spawn("B", dir_b.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    a.wait_for("join request", |v| is_event(v, "join_request"));
    b.wait_for("B joined", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    a.wait_for("B joined on A", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    eprintln!("joined after {:?}", started.elapsed());

    let a_msgs: Vec<String> = (1..=10).map(|i| format!("mac-{i:02}")).collect();
    let b_msgs: Vec<String> = (1..=10).map(|i| format!("linux-{i:02}")).collect();
    for i in 0..10 {
        a.send(&a_msgs[i]);
        b.send(&b_msgs[i]);
    }
    assert_eq!(receive(&mut a, 10), b_msgs, "A got B's messages in order");
    assert_eq!(receive(&mut b, 10), a_msgs, "B got A's messages in order");
    eprintln!("20 messages exchanged after {:?}", started.elapsed());

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
    assert!(extra.is_empty(), "no duplicate deliveries: {extra:?}");

    b.quit();
    a.quit();

    for (dir, name) in [(dir_a.path(), "A"), (dir_b.path(), "B")] {
        let (code, out) = run_cmd(dir, &["history"]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 25, "{name} history:\n{out}");
        for needle in ["mac-01", "linux-01", "mac-15"] {
            let (code, out) = run_cmd(dir, &["inspect", "--grep", needle]);
            assert_eq!(
                code, 0,
                "{name}: plaintext {needle} in p2panda.sqlite: {out}"
            );
        }
        let (_, status) = run_cmd(dir, &["status"]);
        assert!(status.contains("failed=0"), "{name}:\n{status}");
    }
}
