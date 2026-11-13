//! W2: the token and the origin. No token, a wrong token, a late token and
//! a disallowed `Origin` are refused; a non-loopback address needs
//! `--allow-remote`; the token file is the user's alone.

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

#[path = "support/ws.rs"]
mod wsc;
use wsc::{Frame, Server, Ws, solo_channel};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn close_code(ws: &mut Ws, within: Duration) -> Option<u16> {
    match ws.next_frame(within) {
        Some(Frame::Close(code, _)) => Some(code),
        Some(Frame::Text(v)) => panic!("{}: got {v} before any auth", ws.name),
        None => None,
    }
}

#[test]
fn ws_token_and_origin() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(
        BIN,
        dir.path(),
        &["--allow-origin", "http://localhost:5173"],
        &[],
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("api-token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the token file is the user's alone");
    }
    assert_eq!(server.token.len(), 64, "32 random bytes, hex");

    // A wrong token in the header: refused before the upgrade.
    let err = Ws::connect(
        "wrong-header",
        &server.addr,
        &[("Authorization", "Bearer nope")],
    )
    .err()
    .expect("refused");
    assert!(err.contains("401"), "{err}");

    // A web page at an origin nobody allowed: refused, even with the token.
    let auth = format!("Bearer {}", server.token);
    let err = Ws::connect(
        "foreign-origin",
        &server.addr,
        &[("Origin", "http://evil.test"), ("Authorization", &auth)],
    )
    .err()
    .expect("refused");
    assert!(err.contains("403"), "{err}");

    // The allowed origin, authenticating with the first frame as a browser
    // must.
    let mut ok = Ws::connect(
        "allowed-origin",
        &server.addr,
        &[("Origin", "http://localhost:5173")],
    )
    .expect("allowed");
    ok.send(&json!({"op": "auth", "token": server.token}));
    ok.handshake();

    // Another path: not the API.
    let err = {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(&server.addr).unwrap();
        s.write_all(b"GET /other HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    assert!(err.starts_with("HTTP/1.1 404"), "{err}");

    // A wrong token in the first frame: closed, nothing sent.
    let mut wrong = Ws::connect("wrong-frame", &server.addr, &[]).unwrap();
    wrong.send(&json!({"op": "auth", "token": "0".repeat(64)}));
    assert_eq!(close_code(&mut wrong, Duration::from_secs(10)), Some(4001));

    // No token: a request as the first frame is not one.
    let mut none = Ws::connect("no-token", &server.addr, &[]).unwrap();
    none.send(&json!({"id": 1, "op": "send", "channel": "garden", "text": "sneaky"}));
    assert_eq!(close_code(&mut none, Duration::from_secs(10)), Some(4001));

    // A late token: five seconds, then closed.
    let started = Instant::now();
    let mut late = Ws::connect("late", &server.addr, &[]).unwrap();
    assert_eq!(close_code(&mut late, Duration::from_secs(15)), Some(4002));
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(10),
        "closed after {waited:?}"
    );
    late.send(&json!({"op": "auth", "token": server.token}));
    assert!(
        late.next_frame(Duration::from_secs(2)).is_none(),
        "nothing after the close"
    );

    // The good connection is still there and unaffected.
    let (done, _) = ok.call(json!({"op": "snapshot"}));
    assert_eq!(done["ok"], true);
}

#[test]
fn ws_remote_needs_a_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .env("TRIAD_NO_BOT", "1")
        .arg("--data-dir")
        .arg(dir.path())
        .args(["serve", "--listen", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--allow-remote"), "{err}");
    assert!(
        !dir.path().join("api-token").exists(),
        "no token is made for a refused start"
    );
}

/// Sends `bytes` (all at once, or `drip` bytes per second) and returns what
/// the server answers before closing, with how long that took.
fn raw(addr: &str, bytes: &[u8], drip: Option<u32>) -> (String, Duration) {
    use std::io::{Read, Write};
    let started = Instant::now();
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    match drip {
        None => {
            let _ = s.write_all(bytes);
        }
        Some(per_sec) => {
            for b in bytes {
                if s.write_all(&[*b]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1) / per_sec);
            }
        }
    }
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    (
        String::from_utf8_lossy(&out).into_owned(),
        started.elapsed(),
    )
}

#[test]
fn ws_request_head_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);

    // Over 8 KiB of head: refused before any upgrade.
    let big = format!(
        "GET /v1 HTTP/1.1\r\nHost: x\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(9 * 1024)
    );
    let (out, _) = raw(&server.addr, big.as_bytes(), None);
    assert!(out.starts_with("HTTP/1.1 431"), "{out}");
    // Also with no end to the head at all.
    let (out, _) = raw(&server.addr, "a".repeat(20 * 1024).as_bytes(), None);
    assert!(out.starts_with("HTTP/1.1 431"), "{out}");

    // A head that never finishes: refused when the 5 s run out.
    let (out, took) = raw(&server.addr, b"GET /v1 HTTP/1.1\r\nHost: x\r\n", None);
    assert!(out.starts_with("HTTP/1.1 408"), "{out}");
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(10),
        "{took:?}"
    );
    // A head dripped in too slowly to finish in 5 s, though each byte is
    // prompt: the limit is on the whole head, not per read.
    let head = format!(
        "GET /v1 HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
        server.token
    );
    let (out, took) = raw(&server.addr, head.as_bytes(), Some(20));
    assert!(out.starts_with("HTTP/1.1 408"), "{out}");
    assert!(!out.contains("101"), "never upgraded: {out}");
    assert!(took < Duration::from_secs(10), "{took:?}");

    // A large but legal head (7 KiB) with the token still works, and the
    // server is unharmed by the above.
    let pad = "b".repeat(7 * 1024);
    let auth = format!("Bearer {}", server.token);
    let mut ws = Ws::connect(
        "big-ok",
        &server.addr,
        &[("X-Pad", &pad), ("Authorization", &auth)],
    )
    .expect("a 7 KiB head is accepted");
    ws.handshake();
}
