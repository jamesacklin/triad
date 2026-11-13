//! The web app's routes on `triad serve`. With the `web` feature, `GET /`
//! is the app's page (under a Content-Security-Policy, never cached) and
//! `GET /assets/…` its files (cached for good); a page served from the
//! bound loopback address may open the socket without `--allow-origin`.
//! Without the feature, `GET /` says the build has no web app. In both, an
//! `Origin` that only matches a forged `Host` is refused: the allowed
//! origins come from the bound address, never from the request.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[path = "support/ws.rs"]
mod wsc;
#[cfg(feature = "web")]
use wsc::Ws;
use wsc::{Server, solo_channel};

const BIN: &str = env!("CARGO_BIN_EXE_triad");

/// One HTTP/1.1 request; returns the status line, the headers (lowercased
/// names) and the body.
fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {addr}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&out[..split]).to_string();
    let body = out[split + 4..].to_vec();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The status line of a WebSocket upgrade to `/v1` with these headers.
fn upgrade_status(addr: &str, headers: &[(&str, &str)]) -> String {
    let mut all = vec![
        ("Upgrade", "websocket"),
        ("Connection", "Upgrade"),
        ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ("Sec-WebSocket-Version", "13"),
    ];
    all.extend_from_slice(headers);
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = String::from("GET /v1 HTTP/1.1\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {addr}\r\n"));
    }
    for (k, v) in &all {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        match s.read(&mut b) {
            Ok(1) => line.push(b[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&line).trim().to_string()
}

fn port(addr: &str) -> &str {
    addr.rsplit_once(':').unwrap().1
}

#[test]
fn ws_web_an_origin_matching_a_forged_host_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);
    let p = port(&server.addr);
    // A DNS-rebinding page: evil.test now resolves to 127.0.0.1, so the
    // browser sends Host and Origin both naming evil.test.
    let host = format!("evil.test:{p}");
    let origin = format!("http://evil.test:{p}");
    let status = upgrade_status(&server.addr, &[("Host", &host), ("Origin", &origin)]);
    assert!(status.contains(" 403 "), "{status}");
    // The same with the token in a header: still refused.
    let auth = format!("Bearer {}", server.token);
    let status = upgrade_status(
        &server.addr,
        &[
            ("Host", &host),
            ("Origin", &origin),
            ("Authorization", &auth),
        ],
    );
    assert!(status.contains(" 403 "), "{status}");
    // Other paths are as they were.
    let (status, _, _) = http(&server.addr, "GET", "/other", &[]);
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
}

#[cfg(not(feature = "web"))]
#[test]
fn ws_web_without_the_feature_root_says_so() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);
    let (status, headers, body) = http(&server.addr, "GET", "/", &[]);
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
    assert!(
        header(&headers, "content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("no web app"), "{body}");
    assert!(body.contains("--features web"), "{body}");
    let (status, _, _) = http(&server.addr, "GET", "/assets/index.js", &[]);
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
    // No page is served, so no origin is allowed without the flag.
    let p = port(&server.addr);
    let origin = format!("http://127.0.0.1:{p}");
    let status = upgrade_status(&server.addr, &[("Origin", &origin)]);
    assert!(status.contains(" 403 "), "{status}");
    // And no `open` line after `token`.
    let second = server.stdout.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(second.starts_with("token"), "{second}");
    assert!(
        server
            .stdout
            .recv_timeout(Duration::from_millis(500))
            .is_err(),
        "nothing after the token line"
    );
}

#[cfg(feature = "web")]
#[test]
fn ws_web_serves_the_app() {
    let dir = tempfile::tempdir().unwrap();
    solo_channel(BIN, dir.path(), "garden");
    let server = Server::start(BIN, dir.path(), &[], &[]);
    let p = port(&server.addr).to_string();

    let second = server.stdout.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(second.starts_with("token"), "{second}");
    let third = server.stdout.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(third, format!("open       http://{}/", server.addr));

    // The page: HTML, never cached, under the CSP, no token needed.
    let (status, headers, body) = http(&server.addr, "GET", "/", &[]);
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(header(&headers, "cache-control"), Some("no-cache"));
    let csp = header(&headers, "content-security-policy").expect("a CSP");
    for part in [
        "default-src 'none'",
        "script-src 'self'",
        "style-src 'self'",
        "connect-src ws: wss:",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(part), "{csp} lacks {part}");
    }
    assert!(!csp.contains("unsafe"), "{csp}");
    assert_eq!(header(&headers, "x-frame-options"), Some("DENY"));
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains("<div id=\"root\">"), "{html}");

    // Its script and stylesheet, cached for good, with their types.
    let mut seen = 0;
    for (attr, ty) in [("src=\"", "text/javascript"), ("href=\"", "text/css")] {
        let path = html
            .split(attr)
            .skip(1)
            .map(|r| r.split('"').next().unwrap())
            .find(|p| p.starts_with("/assets/"))
            .unwrap_or_else(|| panic!("no {attr} asset in {html}"));
        let (status, headers, body) = http(&server.addr, "GET", path, &[]);
        assert!(status.starts_with("HTTP/1.1 200"), "{path}: {status}");
        assert!(
            header(&headers, "content-type").unwrap().starts_with(ty),
            "{path}"
        );
        assert_eq!(
            header(&headers, "cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert!(!body.is_empty());
        seen += 1;
    }
    assert_eq!(seen, 2);
    let (status, _, _) = http(&server.addr, "GET", "/assets/nope.js", &[]);
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
    let (status, _, _) = http(&server.addr, "POST", "/", &[]);
    assert!(status.starts_with("HTTP/1.1 405"), "{status}");

    // The page's own origins may open the socket without --allow-origin;
    // the token is still required.
    for origin in [
        format!("http://127.0.0.1:{p}"),
        format!("http://localhost:{p}"),
    ] {
        let mut ws = Ws::connect("own-origin", &server.addr, &[("Origin", &origin)])
            .unwrap_or_else(|e| panic!("{origin}: {e}"));
        ws.send(&serde_json::json!({"op": "auth", "token": server.token}));
        ws.handshake();
    }
    // Bound to 127.0.0.1, the IPv6 loopback port is not ours.
    let status = upgrade_status(&server.addr, &[("Origin", &format!("http://[::1]:{p}"))]);
    assert!(status.contains(" 403 "), "{status}");
    // Another port on the same host is another origin.
    let status = upgrade_status(&server.addr, &[("Origin", "http://127.0.0.1:1")]);
    assert!(status.contains(" 403 "), "{status}");
}
