//! Membership changes while members are offline, on p2panda spaces.
//!
//! FINDINGS.md recommends spaces partly because they handle membership
//! changes without a single committer and with members offline. These tests
//! check that claim with separate processes on one machine. "Offline" means
//! the process is stopped, so it can neither send nor receive.
//!
//! 1. A member is added while another is offline.
//! 2. A member is removed while it and another member are offline, and then
//!    writes before it learns of the removal.
//! 3. Two managers who are never online together each add a member.
//! 4. A manager is removed while it adds a member, offline from the remover.
//!    p2panda-auth's "strong removal" rule should invalidate that addition.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use triad_core::envelope::Invite;
use triad_core::net::DEFAULT_RELAY;

#[path = "../../triad-cli/tests/support/mod.rs"]
mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad-spaces");

/// How long to watch for messages that must not arrive.
const QUIET: Duration = Duration::from_secs(10);
/// How long membership may take to converge after nodes reconnect.
const CONVERGE: Duration = Duration::from_secs(90);

struct Node {
    proc: Proc,
    id: String,
}

fn ready(p: &mut Proc) -> Value {
    p.wait_for("ready", |v| is_event(v, "ready"))
}

/// Starts a creator and returns it with its invite and space topic.
fn create(name: &'static str, dir: &Path) -> (Node, String, [u8; 32]) {
    let mut proc = Proc::spawn(name, dir, &["create", "--auto-accept", "--json"]);
    let r = ready(&mut proc);
    let invite = r["invite"].as_str().unwrap().to_string();
    let topic: [u8; 32] = hex::decode(r["group"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let id = r["node"].as_str().unwrap().to_string();
    (Node { proc, id }, invite, topic)
}

/// Joins with `invite`, which the manager `by` (online, auto-accepting) admits.
fn join(name: &'static str, dir: &Path, invite: &str, by: &mut Node) -> Node {
    let mut proc = Proc::spawn(name, dir, &["join", invite, "--json"]);
    let id = ready(&mut proc)["node"].as_str().unwrap().to_string();
    proc.wait_for("self joined", |v| {
        is_event(v, "joined") && v["member"] == id.as_str()
    });
    by.proc.wait_for("member joined", |v| {
        is_event(v, "joined") && v["member"] == id.as_str()
    });
    Node { proc, id }
}

/// Restarts an existing data directory in chat mode.
fn resume(name: &'static str, dir: &Path, auto_accept: bool) -> Node {
    let args: &[&str] = if auto_accept {
        &["chat", "--auto-accept", "--json"]
    } else {
        &["chat", "--json"]
    };
    let mut proc = Proc::spawn(name, dir, args);
    let id = ready(&mut proc)["node"].as_str().unwrap().to_string();
    Node { proc, id }
}

/// An invite that bootstraps from `via` instead of the space's creator.
fn invite_via(topic: [u8; 32], via: &str) -> String {
    let via: [u8; 32] = hex::decode(via).unwrap().try_into().unwrap();
    Invite::new(topic, via, DEFAULT_RELAY).render().unwrap()
}

fn members_now(n: &mut Node) -> (BTreeSet<String>, Value) {
    n.proc.send("/members");
    let v = n.proc.wait_for("members", |v| is_event(v, "members"));
    let set = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    (set, v["access"].clone())
}

/// Polls `/members` until it equals `want`, then returns how long it took.
fn converge<S: AsRef<str>>(n: &mut Node, want: &[S]) -> Duration {
    let want: BTreeSet<String> = want.iter().map(|s| s.as_ref().to_string()).collect();
    let start = Instant::now();
    loop {
        let (got, _) = members_now(n);
        if got == want {
            return start.elapsed();
        }
        assert!(
            start.elapsed() < CONVERGE,
            "{}: members did not converge.\n want {want:?}\n got  {got:?}",
            n.proc.name
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Waits until every text in `want` has arrived as a foreign message.
fn expect_texts(n: &mut Node, want: &[&str]) {
    let mut missing: BTreeSet<String> = want.iter().map(|s| s.to_string()).collect();
    for t in n.proc.seen.iter().filter_map(foreign_text) {
        missing.remove(&t);
    }
    while !missing.is_empty() {
        let what = format!("messages {missing:?}");
        let v = n.proc.wait_for(&what, |v| foreign_text(v).is_some());
        missing.remove(&foreign_text(&v).unwrap());
    }
}

/// Watches for `QUIET` and fails if any text in `forbidden` was ever shown.
fn expect_absent(n: &mut Node, forbidden: &[&str]) {
    n.proc.drain(QUIET);
    let shown: Vec<String> = n
        .proc
        .seen
        .iter()
        .filter_map(foreign_text)
        .filter(|t| forbidden.contains(&t.as_str()))
        .collect();
    assert!(
        shown.is_empty(),
        "{} showed messages it must not: {shown:?}",
        n.proc.name
    );
}

/// Every foreign message this process has shown so far.
fn shown(n: &Node) -> Vec<String> {
    n.proc.seen.iter().filter_map(foreign_text).collect()
}

/// Declines every outstanding join request from a non-member and returns
/// their IDs. The chat takes the next plain line as the answer to a pending
/// request, so a test must answer before it sends a message.
fn decline_pending(n: &mut Node) -> Vec<String> {
    n.proc.drain(Duration::from_secs(2));
    let (members, _) = members_now(n);
    let mut asked: Vec<String> = Vec::new();
    for v in &n.proc.seen {
        if is_event(v, "join_request") {
            let m = v["member"].as_str().unwrap().to_string();
            if !members.contains(&m) && !asked.contains(&m) {
                asked.push(m);
            }
        }
    }
    let declined =
        |v: &Value| is_event(v, "info") && v["text"].as_str().unwrap_or("").starts_with("declined");
    let already = n.proc.seen.iter().filter(|v| declined(v)).count();
    for _ in already..asked.len() {
        n.proc.send("n");
        n.proc.wait_for("declined", declined);
    }
    asked
}

fn send_own(n: &mut Node, text: &str) {
    n.proc.send(text);
    n.proc
        .wait_for("own message", |v| own_text(v).as_deref() == Some(text));
}

fn history(dir: &Path) -> Vec<String> {
    let (code, out) = run_cmd(dir, &["history"]);
    assert_eq!(code, 0);
    out.lines()
        .map(|l| l.split_once(": ").unwrap().1.to_string())
        .collect()
}

#[test]
#[ignore = "membership experiment, about a minute each; run with --ignored"]
fn add_while_member_offline() {
    let (da, db, dc) = (tempdir(), tempdir(), tempdir());
    let (mut a, invite, _) = create("A", da.path());
    let mut b = join("B", db.path(), &invite, &mut a);
    send_own(&mut a, "a-1");
    expect_texts(&mut b, &["a-1"]);

    // B goes offline; A adds C, and A and C talk.
    b.proc.quit();
    let mut c = join("C", dc.path(), &invite, &mut a);
    send_own(&mut a, "a-2");
    send_own(&mut c, "c-1");
    expect_texts(&mut a, &["c-1"]);
    expect_texts(&mut c, &["a-2"]);
    c.proc.drain(Duration::from_secs(3));
    eprintln!("OBSERVE: C sees from before it joined: {:?}", shown(&c));

    // B returns: it learns about C and reads what it missed, from both.
    let back = Instant::now();
    let mut b = resume("B", db.path(), false);
    let abc = [a.id.clone(), b.id.clone(), c.id.clone()];
    let took = converge(&mut b, &abc);
    eprintln!("B saw 3 members after {took:?}");
    expect_texts(&mut b, &["a-2", "c-1"]);
    eprintln!("B caught up after {:?}", back.elapsed());

    // And the others read B's new messages.
    send_own(&mut b, "b-1");
    expect_texts(&mut a, &["b-1"]);
    expect_texts(&mut c, &["b-1"]);
    let abc = [a.id.clone(), b.id.clone(), c.id.clone()];
    converge(&mut a, &abc);
    let abc = [a.id.clone(), b.id.clone(), c.id.clone()];
    converge(&mut c, &abc);

    for n in [a, b, c] {
        n.proc.quit();
    }
    for d in [&da, &db, &dc] {
        let (_, status) = run_cmd(d.path(), &["status"]);
        assert!(status.contains("failed=0"), "{status}");
    }
}

#[test]
#[ignore = "membership experiment, about a minute each; run with --ignored"]
fn remove_while_members_offline() {
    let (da, db, dc) = (tempdir(), tempdir(), tempdir());
    let (mut a, invite, _) = create("A", da.path());
    let mut b = join("B", db.path(), &invite, &mut a);
    let mut c = join("C", dc.path(), &invite, &mut a);
    let abc = [a.id.clone(), b.id.clone(), c.id.clone()];
    converge(&mut b, &abc);
    send_own(&mut c, "c-1");
    expect_texts(&mut a, &["c-1"]);
    expect_texts(&mut b, &["c-1"]);
    let (a_id, b_id, c_id) = (a.id.clone(), b.id.clone(), c.id.clone());

    // C goes offline. A removes C while B watches, then writes.
    c.proc.quit();
    a.proc.send(&format!("/remove {c_id}"));
    a.proc.wait_for("C removed on A", |v| {
        is_event(v, "removed") && v["member"] == c_id.as_str()
    });
    b.proc.wait_for("C removed on B", |v| {
        is_event(v, "removed") && v["member"] == c_id.as_str()
    });
    send_own(&mut a, "a-after-remove");
    expect_texts(&mut b, &["a-after-remove"]);
    a.proc.quit();
    b.proc.quit();

    // C comes back alone, unaware, and writes.
    let mut c = resume("C", dc.path(), false);
    c.proc.wait_for("C sees itself", |v| is_event(v, "joined"));
    c.proc.send("c-after-remove");
    let outcome = c.proc.wait_for("own message or error", |v| {
        own_text(v).as_deref() == Some("c-after-remove") || is_event(v, "error")
    });
    eprintln!("OBSERVE: C writing while unaware of its removal: {outcome}");
    c.proc.quit();

    // Everyone reconnects.
    let mut a = resume("A", da.path(), false);
    let mut b = resume("B", db.path(), false);
    let mut c = resume("C", dc.path(), false);
    let took = converge(&mut a, &[&a_id, &b_id]);
    eprintln!("A converged after {took:?}");
    converge(&mut b, &[&a_id, &b_id]);
    c.proc.drain(Duration::from_secs(5));
    let (c_view, _) = members_now(&mut c);
    eprintln!(
        "OBSERVE: C's member list after reconnecting: {c_view:?}; C saw itself removed: {}",
        c.proc
            .seen
            .iter()
            .any(|v| is_event(v, "removed") && v["member"] == c_id.as_str())
    );

    // A and B never show C's late message; C never reads A's.
    expect_absent(&mut a, &["c-after-remove"]);
    expect_absent(&mut b, &["c-after-remove"]);
    expect_absent(&mut c, &["a-after-remove"]);

    // When C reconnects it announces its key bundle again, which A (a
    // manager) sees as a join request.
    let asked = decline_pending(&mut a);
    eprintln!("OBSERVE: A was asked to admit {asked:?} after the reconnect");
    assert!(
        asked.contains(&c_id),
        "removed C reappears as a join request"
    );

    // New messages reach B but not C.
    send_own(&mut a, "a-final");
    expect_texts(&mut b, &["a-final"]);
    expect_absent(&mut c, &["a-final"]);
    c.proc.send("c-final");
    let after = c.proc.wait_for("own message or error", |v| {
        own_text(v).as_deref() == Some("c-final") || is_event(v, "error")
    });
    eprintln!("OBSERVE: C writing at the end, never told: {after}");
    expect_absent(&mut a, &["c-final"]);
    expect_absent(&mut b, &["c-final"]);
    let (c_view, _) = members_now(&mut c);
    eprintln!(
        "OBSERVE: C's member list at the end: {c_view:?}; C saw itself removed: {}",
        c.proc
            .seen
            .iter()
            .any(|v| is_event(v, "removed") && v["member"] == c_id.as_str())
    );

    for n in [a, b, c] {
        n.proc.quit();
    }
    assert!(!history(db.path()).contains(&"c-after-remove".to_string()));
}

#[test]
#[ignore = "membership experiment, about a minute each; run with --ignored"]
fn concurrent_adds_by_two_managers() {
    let (da, db, dd, de) = (tempdir(), tempdir(), tempdir(), tempdir());
    let (mut a, invite, topic) = create("A", da.path());
    let mut b = join("B", db.path(), &invite, &mut a);
    a.proc.send(&format!("/promote {}", b.id));
    let b_id = b.id.clone();
    b.proc.wait_for("B promoted", |v| {
        is_event(v, "promoted") && v["member"] == b_id.as_str() && v["access"] == "manage"
    });
    let a_id = a.id.clone();

    // From here A and B are never online together until the merge.
    b.proc.quit();
    let mut d = join("D", dd.path(), &invite, &mut a);
    send_own(&mut a, "a-1");
    expect_texts(&mut d, &["a-1"]);
    let d_id = d.id.clone();
    a.proc.quit();
    d.proc.quit();

    let mut b = resume("B", db.path(), true);
    b.proc.wait_for("B sees itself", |v| is_event(v, "joined"));
    let mut e = join("E", de.path(), &invite_via(topic, &b_id), &mut b);
    send_own(&mut b, "b-1");
    expect_texts(&mut e, &["b-1"]);
    let e_id = e.id.clone();
    b.proc.quit();
    e.proc.quit();

    // Merge: everyone should end with the same five members.
    let merge = Instant::now();
    let mut nodes = vec![
        resume("A", da.path(), false),
        resume("B", db.path(), false),
        resume("D", dd.path(), false),
        resume("E", de.path(), false),
    ];
    let all = [a_id.as_str(), b_id.as_str(), d_id.as_str(), e_id.as_str()];
    for n in nodes.iter_mut() {
        converge(n, &all);
    }
    eprintln!("all four converged after {:?}", merge.elapsed());

    // Everyone reads everyone's new messages.
    let finals = ["a-final", "b-final", "d-final", "e-final"];
    for (n, t) in nodes.iter_mut().zip(finals) {
        send_own(n, t);
    }
    for (i, n) in nodes.iter_mut().enumerate() {
        let others: Vec<&str> = finals
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, t)| *t)
            .collect();
        expect_texts(n, &others);
    }
    for n in nodes.iter_mut() {
        n.proc.drain(Duration::from_secs(3));
        eprintln!("OBSERVE: {} shows from others: {:?}", n.proc.name, shown(n));
    }
    for n in nodes {
        n.proc.quit();
    }
}

#[test]
#[ignore = "membership experiment, about a minute each; run with --ignored"]
fn removing_a_manager_invalidates_its_concurrent_add() {
    let (da, db, dc, de) = (tempdir(), tempdir(), tempdir(), tempdir());
    let (mut a, invite, topic) = create("A", da.path());
    let mut b = join("B", db.path(), &invite, &mut a);
    a.proc.send(&format!("/promote {}", b.id));
    let b_id = b.id.clone();
    b.proc.wait_for("B promoted", |v| {
        is_event(v, "promoted") && v["member"] == b_id.as_str() && v["access"] == "manage"
    });
    let mut c = join("C", dc.path(), &invite, &mut a);
    let abc = [a.id.clone(), b.id.clone(), c.id.clone()];
    converge(&mut b, &abc);
    let (a_id, c_id) = (a.id.clone(), c.id.clone());

    // B goes offline. A removes B, with C watching.
    b.proc.quit();
    a.proc.send(&format!("/remove {b_id}"));
    c.proc.wait_for("B removed on C", |v| {
        is_event(v, "removed") && v["member"] == b_id.as_str()
    });
    send_own(&mut a, "a-after-remove");
    expect_texts(&mut c, &["a-after-remove"]);
    a.proc.quit();
    c.proc.quit();

    // Concurrently, B (unaware) adds E and they talk.
    let mut b = resume("B", db.path(), true);
    b.proc.wait_for("B sees itself", |v| is_event(v, "joined"));
    let mut e = join("E", de.path(), &invite_via(topic, &b_id), &mut b);
    send_own(&mut b, "b-concurrent");
    send_own(&mut e, "e-1");
    expect_texts(&mut b, &["e-1"]);
    expect_texts(&mut e, &["b-concurrent"]);
    b.proc.quit();
    e.proc.quit();

    // Merge. Strong removal: B's addition of E is invalid, so only A and C remain.
    let merge = Instant::now();
    let mut a = resume("A", da.path(), false);
    let mut c = resume("C", dc.path(), false);
    let mut b = resume("B", db.path(), false);
    let mut e = resume("E", de.path(), false);
    converge(&mut a, &[&a_id, &c_id]);
    converge(&mut c, &[&a_id, &c_id]);
    eprintln!("A and C converged after {:?}", merge.elapsed());
    // B and E converge on the same answer: neither of them is a member.
    let took = converge(&mut b, &[&a_id, &c_id]);
    eprintln!("B's member list matched A's after {took:?}");
    let took = converge(&mut e, &[&a_id, &c_id]);
    eprintln!("E's member list matched A's after {took:?}");
    for (name, n) in [("A", &mut a), ("C", &mut c)] {
        expect_absent(n, &["e-1"]);
        eprintln!(
            "OBSERVE: {name} shows B's concurrent message: {}",
            shown(n).contains(&"b-concurrent".to_string())
        );
    }

    // B and E reappear on A as join requests; decline them.
    let asked = decline_pending(&mut a);
    eprintln!("OBSERVE: A was asked to admit {asked:?} after the merge");

    // After the merge, B and E can no longer read the group.
    send_own(&mut a, "a-final");
    expect_texts(&mut c, &["a-final"]);
    expect_absent(&mut b, &["a-final"]);
    expect_absent(&mut e, &["a-final"]);

    for n in [a, b, c, e] {
        n.proc.quit();
    }
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
