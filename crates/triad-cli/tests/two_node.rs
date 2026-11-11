//! Two `triad` processes, two data directories, one machine.
//!
//! Exercises create, join, 10 messages each way in order, kill -9 of the
//! joiner, 5 messages while it is down, and catch-up after restart.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_triad");
const TIMEOUT: Duration = Duration::from_secs(120);

/// Whether message bodies are encrypted before p2panda sees them.
const ENCRYPTED: bool = true;

struct Proc {
    name: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    seen: Vec<Value>,
}

impl Proc {
    fn spawn(name: &'static str, dir: &Path, args: &[&str]) -> Proc {
        Self::spawn_env(name, dir, args, &[])
    }

    fn spawn_env(name: &'static str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Proc {
        let mut child = Command::new(BIN)
            .envs(env.iter().copied())
            .arg("--data-dir")
            .arg(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn triad");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Value>(&line) {
                    Ok(v) => {
                        eprintln!("[{name}] {line}");
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(_) => eprintln!("[{name}] (non-json) {line}"),
                }
            }
        });
        let stdin = child.stdin.take();
        Proc {
            name,
            child,
            stdin,
            rx,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for the first new event matching `pred`.
    fn wait_for(&mut self, what: &str, mut pred: impl FnMut(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(v) => {
                    self.seen.push(v.clone());
                    if pred(&v) {
                        return v;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("{}: timed out waiting for {what}", self.name)
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("{}: exited while waiting for {what}", self.name)
                }
            }
        }
    }

    /// Collects every event that arrives within `window`.
    fn drain(&mut self, window: Duration) -> Vec<Value> {
        let deadline = Instant::now() + window;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left) {
                Ok(v) => {
                    self.seen.push(v.clone());
                    out.push(v);
                }
                Err(_) => break,
            }
        }
        out
    }

    fn kill9(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn quit(mut self) {
        self.send("/quit");
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{} exited with {status}", self.name);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        panic!("{} did not exit after /quit", self.name);
    }
}

fn is_event(v: &Value, name: &str) -> bool {
    v["event"] == name
}

fn foreign_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["mine"] == false).then(|| v["text"].as_str().unwrap().to_string())
}

fn own_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["mine"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// Waits until `n` foreign messages arrived and returns their texts in arrival order.
fn receive(p: &mut Proc, n: usize) -> Vec<String> {
    let mut got: Vec<String> = p.seen.iter().filter_map(foreign_text).collect();
    while got.len() < n {
        let v = p.wait_for("messages", |v| foreign_text(v).is_some());
        got.push(foreign_text(&v).unwrap());
    }
    got
}

fn run_cmd(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run triad");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn wait_joined(p: &mut Proc, ready: &Value) {
    if ready["status"] == "pending" {
        p.wait_for("joined", |v| is_event(v, "joined"));
    }
}

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
