//! Helpers shared by the integration tests. Each test crate defines
//! `const BIN` at its root.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

pub const TIMEOUT: Duration = Duration::from_secs(120);

pub struct Proc {
    pub name: &'static str,
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub rx: Receiver<Value>,
    pub seen: Vec<Value>,
}

impl Proc {
    pub fn spawn(name: &'static str, dir: &Path, args: &[&str]) -> Proc {
        Self::spawn_env(name, dir, args, &[])
    }

    /// Bots run by default. So a model key in the developer's environment
    /// never reaches a test, a process runs without bots (`TRIAD_NO_BOT`)
    /// unless its test chooses a brain or model, or sets `TRIAD_NO_BOT`.
    pub fn spawn_env(name: &'static str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Proc {
        let mut cmd = Command::new(crate::BIN);
        if !env
            .iter()
            .any(|(k, _)| ["TRIAD_AGENT_BRAIN", "TRIAD_AGENT_MODEL", "TRIAD_NO_BOT"].contains(k))
        {
            cmd.env("TRIAD_NO_BOT", "1");
        }
        let mut child = cmd
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

    pub fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for the first new event matching `pred`.
    pub fn wait_for(&mut self, what: &str, mut pred: impl FnMut(&Value) -> bool) -> Value {
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
    pub fn drain(&mut self, window: Duration) -> Vec<Value> {
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

    pub fn kill9(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    pub fn quit(mut self) {
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

impl Drop for Proc {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn is_event(v: &Value, name: &str) -> bool {
    v["event"] == name
}

pub fn foreign_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["mine"] == false).then(|| v["text"].as_str().unwrap().to_string())
}

pub fn own_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["mine"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// Waits until `n` foreign messages arrived and returns their texts in arrival order.
pub fn receive(p: &mut Proc, n: usize) -> Vec<String> {
    let mut got: Vec<String> = p.seen.iter().filter_map(foreign_text).collect();
    while got.len() < n {
        let v = p.wait_for("messages", |v| foreign_text(v).is_some());
        got.push(foreign_text(&v).unwrap());
    }
    got
}

pub fn run_cmd(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(crate::BIN)
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

pub fn wait_joined(p: &mut Proc, ready: &Value) {
    if ready["status"] == "pending" {
        p.wait_for("joined", |v| is_event(v, "joined"));
    }
}
