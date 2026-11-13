//! Frame limits and control frames (AXUM-PLAN.md, A2): a message over
//! 1 MiB closes with `1009`, a binary frame closes with `1003`, and a
//! client's ping is answered with a pong carrying its payload.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[path = "support/ws.rs"]
mod wsc;
use wsc::{Frame, Server, solo_channel};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

#[test]
fn ws_message_over_1_mib_closes_1009() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);

    // Exactly 1 MiB is read: it gets a reply (an error for the unknown
    // field is fine), not a close.
    let mut ok = server.connect("at-limit");
    ok.handshake();
    let empty = r#"{"id":1,"op":"snapshot","pad":""}"#;
    let pad = "x".repeat(1024 * 1024 - empty.len());
    let at_limit = format!(r#"{{"id":1,"op":"snapshot","pad":"{pad}"}}"#);
    assert_eq!(at_limit.len(), 1024 * 1024);
    ok.send_text(&at_limit);
    ok.wait_for("done", |v| v["re"] == 1 && v["event"] == "done");

    // One byte more is not.
    let mut big = server.connect("over-limit");
    big.handshake();
    big.send_text(&"x".repeat(1024 * 1024 + 1));
    let deadline = Instant::now() + Duration::from_secs(20);
    let code = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match big.next_frame(left) {
            Some(Frame::Close(code, _)) => break Some(code),
            Some(Frame::Text(_)) => continue,
            None => break None,
        }
    };
    assert_eq!(code, Some(1009));

    // The other client is unaffected.
    let (done, _) = ok.call(serde_json::json!({"op": "snapshot"}));
    assert_eq!(done["ok"], true);
}

/// A masked client frame.
fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126);
    let mask = [0x0a, 0x0b, 0x0c, 0x0d];
    let mut f = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// Reads frames until one with `opcode`, returning its payload.
fn read_until(s: &mut TcpStream, opcode: u8) -> Option<Vec<u8>> {
    loop {
        let mut h = [0u8; 2];
        s.read_exact(&mut h).ok()?;
        let mut len = (h[1] & 0x7f) as usize;
        if len == 126 {
            let mut l = [0u8; 2];
            s.read_exact(&mut l).ok()?;
            len = u16::from_be_bytes(l) as usize;
        } else if len == 127 {
            let mut l = [0u8; 8];
            s.read_exact(&mut l).ok()?;
            len = u64::from_be_bytes(l) as usize;
        }
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).ok()?;
        if h[0] & 0x0f == opcode {
            return Some(payload);
        }
    }
}

#[test]
fn ws_ping_is_answered_and_binary_closes_1003() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);

    let mut s = TcpStream::connect(&server.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let head = format!(
        "GET /v1 HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
         Authorization: Bearer {}\r\n\r\n",
        server.token
    );
    s.write_all(head.as_bytes()).unwrap();
    let mut got = Vec::new();
    let mut b = [0u8; 1];
    while !got.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        got.push(b[0]);
    }
    assert!(got.starts_with(b"HTTP/1.1 101"), "{got:?}");

    s.write_all(&frame(0x9, b"are you there")).unwrap();
    assert_eq!(
        read_until(&mut s, 0xA).as_deref(),
        Some(&b"are you there"[..])
    );

    s.write_all(&frame(0x2, b"\x00\x01")).unwrap();
    let close = read_until(&mut s, 0x8).expect("a close frame");
    assert_eq!(u16::from_be_bytes([close[0], close[1]]), 1003);
}
