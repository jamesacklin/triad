//! A small blocking WebSocket client and a `triad serve` process for the
//! socket tests. Written against RFC 6455 directly so the tests can also
//! send what a real client library would not (no token, a foreign
//! `Origin`, a late token) and read the HTTP refusal.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const TIMEOUT: Duration = Duration::from_secs(120);

/// A `triad serve` process.
pub struct Server {
    pub child: Child,
    pub addr: String,
    pub token: String,
    pub stdout: Receiver<String>,
}

impl Server {
    /// Starts `serve` on a free loopback port and waits for its
    /// `listening` line. Bots are off unless `env` chooses them.
    pub fn start(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Server {
        Self::try_start(bin, dir, args, env).unwrap_or_else(|e| panic!("serve: {e}"))
    }

    pub fn try_start(
        bin: &str,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Server, String> {
        let mut cmd = Command::new(bin);
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
            .arg("serve")
            .args(["--listen", "127.0.0.1:0"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn triad serve");
        let (tx, rx) = channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                eprintln!("[serve] {line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = child.stderr.take().unwrap();
        let (etx, erx) = channel();
        std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("[serve stderr] {line}");
                all.push_str(&line);
                all.push('\n');
            }
            let _ = etx.send(all);
        });
        let line = match rx.recv_timeout(TIMEOUT) {
            Ok(line) => line,
            Err(_) => {
                let _ = child.wait();
                return Err(erx.recv_timeout(TIMEOUT).unwrap_or_default());
            }
        };
        let addr = line
            .strip_prefix("listening  ws://")
            .and_then(|r| r.strip_suffix("/v1"))
            .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
            .to_string();
        let token = std::fs::read_to_string(dir.join("api-token"))
            .unwrap()
            .trim()
            .to_string();
        Ok(Server {
            child,
            addr,
            token,
            stdout: rx,
        })
    }

    /// Connects with the token in the `Authorization` header.
    pub fn connect(&self, name: &'static str) -> Ws {
        let auth = format!("Bearer {}", self.token);
        Ws::connect(name, &self.addr, &[("Authorization", &auth)]).expect("connect")
    }

    /// Connects and authenticates with the first frame, as a browser does.
    pub fn connect_frame(&self, name: &'static str) -> Ws {
        let mut ws = Ws::connect(name, &self.addr, &[]).expect("connect");
        ws.send(&json!({"op": "auth", "token": self.token}));
        ws
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the server sent.
#[derive(Debug)]
pub enum Frame {
    Text(Value),
    Close(u16, String),
}

pub struct Ws {
    pub name: &'static str,
    stream: TcpStream,
    pub seen: Vec<Value>,
    next_id: u64,
}

impl Ws {
    /// Opens the socket. `Err` carries the HTTP status line of a refusal.
    pub fn connect(name: &'static str, addr: &str, headers: &[(&str, &str)]) -> Result<Ws, String> {
        let mut stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
        let mut req = format!(
            "GET /v1 HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        // Read the response head byte by byte so no frame bytes are lost.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut b) {
                Ok(1) => head.push(b[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head).to_string();
        let status = head.lines().next().unwrap_or_default().to_string();
        if !status.contains(" 101 ") {
            return Err(status);
        }
        Ok(Ws {
            name,
            stream,
            seen: Vec::new(),
            next_id: 0,
        })
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) {
        let mut frame = vec![0x80 | opcode];
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask = [0x12, 0x34, 0x56, 0x78];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let _ = self.stream.write_all(&frame);
    }

    pub fn send_text(&mut self, text: &str) {
        self.write_frame(1, text.as_bytes());
    }

    pub fn send(&mut self, v: &Value) {
        self.send_text(&v.to_string());
    }

    /// Sends a request with a fresh `id`, returned.
    pub fn request(&mut self, mut v: Value) -> u64 {
        self.next_id += 1;
        v["id"] = json!(self.next_id);
        self.send(&v);
        self.next_id
    }

    /// The next frame; pings are answered and skipped. `None` on EOF or
    /// when `timeout` passes.
    pub fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        self.stream.set_read_timeout(Some(timeout)).unwrap();
        loop {
            let mut payload = Vec::new();
            let mut opcode = 0;
            loop {
                let mut h = [0u8; 2];
                self.stream.read_exact(&mut h).ok()?;
                let fin = h[0] & 0x80 != 0;
                let op = h[0] & 0x0f;
                let mut len = (h[1] & 0x7f) as u64;
                if len == 126 {
                    let mut l = [0u8; 2];
                    self.stream.read_exact(&mut l).ok()?;
                    len = u16::from_be_bytes(l) as u64;
                } else if len == 127 {
                    let mut l = [0u8; 8];
                    self.stream.read_exact(&mut l).ok()?;
                    len = u64::from_be_bytes(l);
                }
                let mut part = vec![0u8; len as usize];
                self.stream.read_exact(&mut part).ok()?;
                if op >= 8 {
                    match op {
                        9 => self.write_frame(10, &part),
                        8 => {
                            let code = if part.len() >= 2 {
                                u16::from_be_bytes([part[0], part[1]])
                            } else {
                                1005
                            };
                            let reason =
                                String::from_utf8_lossy(part.get(2..).unwrap_or(&[])).to_string();
                            return Some(Frame::Close(code, reason));
                        }
                        _ => {}
                    }
                    continue;
                }
                if op != 0 {
                    opcode = op;
                }
                payload.extend_from_slice(&part);
                if fin {
                    break;
                }
            }
            if opcode == 1 {
                let v: Value = serde_json::from_slice(&payload).expect("a JSON text frame");
                eprintln!("[{}] {v}", self.name);
                return Some(Frame::Text(v));
            }
        }
    }

    /// The next JSON event, or `None` on a close, EOF or timeout.
    pub fn next_event(&mut self, timeout: Duration) -> Option<Value> {
        match self.next_frame(timeout)? {
            Frame::Text(v) => {
                self.seen.push(v.clone());
                Some(v)
            }
            Frame::Close(..) => None,
        }
    }

    /// Waits for the first new event matching `pred`.
    pub fn wait_for(&mut self, what: &str, mut pred: impl FnMut(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!("{}: timed out waiting for {what}", self.name);
            }
            match self.next_event(left) {
                Some(v) if pred(&v) => return v,
                Some(_) => {}
                None => panic!("{}: socket ended waiting for {what}", self.name),
            }
        }
    }

    /// Every event that arrives within `window`.
    pub fn drain(&mut self, window: Duration) -> Vec<Value> {
        let deadline = Instant::now() + window;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.next_event(left) {
                Some(v) => out.push(v),
                None => break,
            }
        }
        out
    }

    /// Waits for `done` for request `id` and returns it, with every event
    /// that carried the same `re` before it.
    pub fn wait_done(&mut self, id: u64) -> (Value, Vec<Value>) {
        let mut replies = Vec::new();
        loop {
            let v = self.wait_for("a reply", |v| v["re"] == id);
            if v["event"] == "done" {
                return (v, replies);
            }
            replies.push(v);
        }
    }

    /// Sends a request and waits for its `done`.
    pub fn call(&mut self, v: Value) -> (Value, Vec<Value>) {
        let id = self.request(v);
        self.wait_done(id)
    }

    /// Waits for `hello` and `ready`, returning `ready`.
    pub fn handshake(&mut self) -> Value {
        let hello = self.wait_for("hello", |v| v["event"] == "hello");
        assert_eq!(hello["protocol"], 1);
        self.wait_for("ready", |v| v["event"] == "ready")
    }
}

/// Makes a data directory with one channel, `name`, this node its creator,
/// by running `create` once.
pub fn solo_channel(bin: &str, dir: &Path, name: &str) {
    let status = Command::new(bin)
        .env("TRIAD_NO_BOT", "1")
        .arg("--data-dir")
        .arg(dir)
        .args(["create", "--name", name, "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .and_then(|mut child| {
            let stdout = child.stdout.take().unwrap();
            for line in BufReader::new(stdout).lines() {
                let line = line?;
                if line.contains("\"event\":\"ready\"") {
                    break;
                }
            }
            writeln!(child.stdin.as_mut().unwrap(), "/quit")?;
            child.wait()
        })
        .expect("create");
    assert!(status.success(), "create exited with {status}");
}

/// Waits until this node is in `channel`'s space, which opens shortly
/// after `ready`; sends before that are refused.
pub fn wait_open(ws: &mut Ws, channel: &str, me: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (done, replies) =
            ws.call(json!({"op": "command", "channel": channel, "line": "/members"}));
        assert_eq!(done["ok"], true, "{done}");
        let members = &replies.iter().find(|v| v["event"] == "members").unwrap()["members"];
        if members.as_array().unwrap().iter().any(|m| m == me) {
            return;
        }
        assert!(Instant::now() < deadline, "#{channel} never opened");
        std::thread::sleep(Duration::from_millis(200));
    }
}
