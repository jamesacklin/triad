//! `triad serve`: a chat session with no terminal, on a WebSocket.
//!
//! The listener reads each connection's HTTP request head itself, so it
//! can refuse a bad path, a foreign `Origin` or a wrong bearer token with
//! an HTTP error before anything is upgraded; tokio-websockets' own
//! `accept` writes `101 Switching Protocols` before it hands back the
//! request. The bytes read are then replayed into `accept`, which does the
//! handshake as usual. See API.md for the protocol and WEBSOCKET-PLAN.md
//! for the design.

mod hub;
pub mod proto;
mod requests;

use std::cell::RefCell;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use futures_sink::Sink as _;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, timeout_at};
use tokio_stream::StreamExt;
use tokio_websockets::{CloseCode, Limits, Message, ServerBuilder, WebSocketStream};
use triad_core::identity::Identity;

pub use hub::{Hub, HubMsg};
pub use proto::{ClientId, Request, Target};

use crate::{BotStart, Input, Opts, Sink, Start};

/// The token file in the data directory.
pub const TOKEN_FILE: &str = "api-token";
/// The one path the socket answers on.
pub const PATH: &str = "/v1";
/// Time from connecting to presenting the token.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the server pings; a client silent for three of these is
/// dropped.
const PING_EVERY: Duration = Duration::from_secs(30);
/// The largest frame (message) a client may send.
pub const MAX_FRAME: usize = 1024 * 1024;
/// The largest HTTP request head the listener reads; a longer one is
/// refused with 431.
pub const MAX_HEAD: usize = 8 * 1024;

/// Close codes the server sends (API.md, "Close codes").
pub const CLOSE_UNAUTHORIZED: u16 = 4001;
pub const CLOSE_AUTH_TIMEOUT: u16 = 4002;
pub const CLOSE_LAGGED: u16 = 4008;

/// `triad serve`'s socket options.
pub struct Listen {
    pub addr: String,
    pub allow_remote: bool,
    pub allow_origins: Vec<String>,
}

/// The request being handled, if any, and the last error emitted under it;
/// part of the printer.
#[derive(Default)]
pub struct Current {
    target: RefCell<Option<Target>>,
    error: RefCell<Option<String>>,
}

impl Current {
    pub fn begin(&self, target: Target) {
        *self.target.borrow_mut() = Some(target);
        *self.error.borrow_mut() = None;
    }

    /// The last error emitted under the request so far.
    pub fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }

    pub fn end(&self) {
        *self.target.borrow_mut() = None;
        *self.error.borrow_mut() = None;
    }

    /// Sends one event to the hub, tagged with the current request.
    pub fn emit(&self, hub: &Hub, value: Value) {
        let target = self.target.borrow().clone();
        if target.is_some() && value["event"] == "error" {
            *self.error.borrow_mut() = Some(value["text"].as_str().unwrap_or("error").to_owned());
        }
        let _ = hub.send(HubMsg::Event { value, target });
    }
}

/// The API token: read from `<data-dir>/api-token`, or created there
/// (mode 0600) on the first `serve`.
pub fn load_or_create_token(data_dir: &Path) -> Result<(String, PathBuf)> {
    let path = data_dir.join(TOKEN_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path)?.permissions().mode();
                if mode & 0o077 != 0 {
                    bail!(
                        "{} can be read by other users (mode {:o}); run chmod 600 on it",
                        path.display(),
                        mode & 0o777
                    );
                }
            }
            let token = text.trim().to_owned();
            if token.len() < 32 {
                bail!(
                    "{} holds no usable token; delete it to make a new one",
                    path.display()
                );
            }
            Ok((token, path))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write;
            let token = hex::encode(triad_core::long::random_key()?);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(&path)
                .with_context(|| format!("creating {}", path.display()))?
                .write_all(format!("{token}\n").as_bytes())?;
            Ok((token, path))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Compares two tokens in time that depends only on their lengths.
fn same_token(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Runs `triad serve`: binds the socket, prints where it is, and runs a
/// chat session whose only input and output is the socket.
pub fn serve(
    rt: &tokio::runtime::Runtime,
    data_dir: &Path,
    listen: Listen,
    channel: Option<String>,
    opts: Opts,
    bots: BotStart,
) -> Result<()> {
    let addrs: Vec<SocketAddr> = listen
        .addr
        .to_socket_addrs()
        .with_context(|| format!("--listen {}: not an address", listen.addr))?
        .collect();
    if addrs.is_empty() {
        bail!("--listen {}: resolves to no address", listen.addr);
    }
    let remote = addrs.iter().any(|a| !a.ip().is_loopback());
    if remote && !listen.allow_remote {
        bail!(
            "--listen {} is not a loopback address; the socket is not encrypted and the token \
             gives full access, so pass --allow-remote to listen there anyway",
            listen.addr
        );
    }
    let (token, token_path) = load_or_create_token(data_dir)?;
    let std_listener = std::net::TcpListener::bind(&addrs[..])
        .with_context(|| format!("listening on {}", listen.addr))?;
    std_listener.set_nonblocking(true)?;
    let local = std_listener.local_addr()?;
    {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        writeln!(out, "listening  ws://{local}{PATH}")?;
        writeln!(out, "token      {}", token_path.display())?;
        if remote {
            writeln!(
                out,
                "warning    not encrypted: anyone on the network path can read the token and \
                 every message; use an SSH tunnel or a TLS proxy"
            )?;
        }
        out.flush()?;
    }
    tracing::info!(%local, "websocket API listening");
    let node = Identity::load_or_create(data_dir)?.node_hex();
    rt.block_on(async move {
        let hub = hub::spawn(node, crate::VERSION);
        let (input, inputs) = mpsc::channel(64);
        let listener = TcpListener::from_std(std_listener)?;
        let shared = Arc::new(Shared {
            hub: hub.clone(),
            input,
            token,
            origins: listen.allow_origins,
        });
        tokio::spawn(accept_loop(listener, shared));
        crate::chat(
            data_dir,
            Start::Resume { channel },
            opts,
            bots,
            Sink::Ws(hub),
            Some(inputs),
        )
        .await
    })
}

struct Shared {
    hub: Hub,
    input: mpsc::Sender<Input>,
    token: String,
    origins: Vec<String>,
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let mut next: ClientId = 0;
    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                next += 1;
                let client = next;
                tracing::debug!(client, %peer, "websocket connection");
                let _ = tcp.set_nodelay(true);
                tokio::spawn(connection(tcp, client, shared.clone()));
            }
            Err(e) => {
                tracing::warn!("accepting a websocket connection: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// An HTTP request head, as far as the listener needs it.
struct Head {
    method: String,
    path: String,
    /// Names lowercased.
    headers: Vec<(String, String)>,
}

impl Head {
    fn parse(bytes: &[u8]) -> Option<Head> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let method = first.next()?.to_owned();
        let path = first.next()?.to_owned();
        first.next().filter(|v| v.starts_with("HTTP/1."))?;
        let mut headers = Vec::new();
        for line in lines.take_while(|l| !l.is_empty()) {
            let (name, value) = line.split_once(':')?;
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
        Some(Head {
            method,
            path,
            headers,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What reading a request head found.
enum HeadRead {
    /// Every byte read, and where the head ends (there may be more after).
    Head(Vec<u8>, usize),
    /// No blank line within [`MAX_HEAD`] bytes.
    TooLarge,
    /// The client closed or the read failed.
    Gone,
}

/// Reads up to and including the blank line that ends the request head,
/// never more than [`MAX_HEAD`] bytes of head.
async fn read_head(tcp: &mut TcpStream) -> HeadRead {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => return HeadRead::Gone,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        match buf.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(end) if end + 4 <= MAX_HEAD => return HeadRead::Head(buf, end + 4),
            Some(_) => return HeadRead::TooLarge,
            None if buf.len() >= MAX_HEAD => return HeadRead::TooLarge,
            None => {}
        }
    }
}

async fn refuse(mut tcp: TcpStream, status: &str, why: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{why}",
        why.len()
    );
    let _ = tcp.write_all(response.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// A stream that first returns bytes already read from it.
struct Replay<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Replay<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let n = (this.prefix.len() - this.pos).min(buf.remaining());
            buf.put_slice(&this.prefix[this.pos..this.pos + n]);
            this.pos += n;
            if this.pos == this.prefix.len() {
                this.prefix = Vec::new();
                this.pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Replay<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

type Ws = WebSocketStream<Replay<TcpStream>>;

/// Sends one message and flushes it.
async fn send(ws: &mut Ws, message: Message) -> Result<(), tokio_websockets::Error> {
    std::future::poll_fn(|cx| Pin::new(&mut *ws).poll_ready(cx)).await?;
    Pin::new(&mut *ws).start_send(message)?;
    std::future::poll_fn(|cx| Pin::new(&mut *ws).poll_flush(cx)).await
}

async fn close(ws: &mut Ws, code: u16, reason: &str) {
    let code = CloseCode::try_from(code).ok();
    let _ = send(ws, Message::close(code, reason)).await;
}

/// Whether a client's `Origin` was allowed with `--allow-origin`.
fn origin_allowed(origins: &[String], origin: &str) -> bool {
    origins
        .iter()
        .any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(origin))
}

/// One client, from the TCP connection to the close.
async fn connection(mut tcp: TcpStream, client: ClientId, shared: Arc<Shared>) {
    let deadline = Instant::now() + AUTH_TIMEOUT;
    // The head shares the 5 s the token has to arrive in.
    let (bytes, end) = match timeout_at(deadline, read_head(&mut tcp)).await {
        Ok(HeadRead::Head(bytes, end)) => (bytes, end),
        Ok(HeadRead::TooLarge) => {
            return refuse(
                tcp,
                "431 Request Header Fields Too Large",
                "the request head is limited to 8 KiB\n",
            )
            .await;
        }
        Ok(HeadRead::Gone) => return,
        Err(_) => {
            return refuse(tcp, "408 Request Timeout", "no request within 5 seconds\n").await;
        }
    };
    let Some(head) = Head::parse(&bytes[..end]) else {
        return refuse(tcp, "400 Bad Request", "not an HTTP request\n").await;
    };
    if head.method != "GET" {
        return refuse(tcp, "405 Method Not Allowed", "use GET\n").await;
    }
    if head.path != PATH {
        return refuse(tcp, "404 Not Found", "the API is at /v1\n").await;
    }
    if let Some(origin) = head.header("origin")
        && !origin_allowed(&shared.origins, origin)
    {
        tracing::info!(client, origin, "refused a websocket from a web page");
        return refuse(
            tcp,
            "403 Forbidden",
            "this origin is not allowed; start serve with --allow-origin\n",
        )
        .await;
    }
    let mut authed = false;
    if let Some(auth) = head.header("authorization") {
        match auth.strip_prefix("Bearer ") {
            Some(t) if same_token(t.trim().as_bytes(), shared.token.as_bytes()) => authed = true,
            _ => {
                tracing::info!(client, "refused a websocket with a wrong token");
                return refuse(tcp, "401 Unauthorized", "wrong token\n").await;
            }
        }
    }
    let stream = Replay {
        prefix: bytes,
        pos: 0,
        inner: tcp,
    };
    let builder = ServerBuilder::new().limits(Limits::default().max_payload_len(Some(MAX_FRAME)));
    let mut ws = match timeout_at(deadline, builder.accept(stream)).await {
        Ok(Ok((_, ws))) => ws,
        Ok(Err(e)) => {
            tracing::debug!(client, "websocket handshake failed: {e}");
            return;
        }
        Err(_) => return,
    };
    if !authed {
        loop {
            match timeout_at(deadline, ws.next()).await {
                Err(_) => {
                    return close(&mut ws, CLOSE_AUTH_TIMEOUT, "no token within 5 seconds").await;
                }
                Ok(Some(Ok(m))) if m.is_ping() || m.is_pong() => continue,
                Ok(Some(Ok(m))) => {
                    let ok = m
                        .as_text()
                        .and_then(proto::auth_token)
                        .is_some_and(|t| same_token(t.as_bytes(), shared.token.as_bytes()));
                    if ok {
                        break;
                    }
                    tracing::info!(client, "refused a websocket with a wrong token");
                    return close(&mut ws, CLOSE_UNAUTHORIZED, "unauthorized").await;
                }
                Ok(_) => return,
            }
        }
    }
    let (frames_tx, mut frames) = mpsc::channel(hub::QUEUE);
    let (lagged_tx, mut lagged) = oneshot::channel();
    if shared
        .hub
        .send(HubMsg::Join {
            client,
            frames: frames_tx,
            lagged: lagged_tx,
        })
        .is_err()
        || shared.input.send(Input::Connected(client)).await.is_err()
    {
        return;
    }
    tracing::info!(client, "websocket client connected");
    run(&mut ws, client, &shared, &mut frames, &mut lagged).await;
    let _ = shared.hub.send(HubMsg::Leave(client));
    tracing::info!(client, "websocket client gone");
}

async fn run(
    ws: &mut Ws,
    client: ClientId,
    shared: &Shared,
    frames: &mut mpsc::Receiver<String>,
    lagged: &mut oneshot::Receiver<u64>,
) {
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    let mut heard = Instant::now();
    let mut lag_gone = false;
    loop {
        tokio::select! {
            incoming = ws.next() => {
                heard = Instant::now();
                match incoming {
                    Some(Ok(m)) if m.is_text() => {
                        let text = m.as_text().unwrap_or_default();
                        match proto::parse(text) {
                            Ok((id, op)) => {
                                let request = Request { client, id, op };
                                if shared.input.send(Input::Request(request)).await.is_err() {
                                    return close(ws, 1001, "the session ended").await;
                                }
                            }
                            Err(bad) => {
                                let target = Target { client, re: bad.id };
                                let _ = shared.hub.send(HubMsg::Event {
                                    value: proto::done(Some(&bad.error)),
                                    target: Some(target),
                                });
                            }
                        }
                    }
                    Some(Ok(m)) if m.is_binary() => {
                        return close(ws, 1003, "send text frames").await;
                    }
                    Some(Ok(m)) if m.is_close() => return,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(client, "websocket read failed: {e}");
                        if matches!(e, tokio_websockets::Error::PayloadTooLong { .. }) {
                            close(ws, 1009, "a frame is limited to 1 MiB").await;
                        }
                        return;
                    }
                    None => return,
                }
            }
            frame = frames.recv() => match frame {
                Some(text) => {
                    if send(ws, Message::text(text)).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            seq = &mut *lagged, if !lag_gone => {
                lag_gone = true;
                if let Ok(seq) = seq {
                    // Frames already queued are delivered first.
                    while let Ok(text) = frames.try_recv() {
                        if send(ws, Message::text(text)).await.is_err() {
                            return;
                        }
                    }
                    let note = json!({"event": "lagged", "seq": seq}).to_string();
                    let _ = send(ws, Message::text(note)).await;
                    return close(ws, CLOSE_LAGGED, "too far behind; resync").await;
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > PING_EVERY * 3 {
                    return close(ws, 1001, "no answer to pings").await;
                }
                if send(ws, Message::ping("")).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_head() {
        let head = Head::parse(
            b"GET /v1 HTTP/1.1\r\nHost: x\r\nOrigin: http://a.test\r\nAuthorization: Bearer t\r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.method, "GET");
        assert_eq!(head.path, "/v1");
        assert_eq!(head.header("origin"), Some("http://a.test"));
        assert_eq!(head.header("authorization"), Some("Bearer t"));
        assert!(Head::parse(b"garbage\r\n\r\n").is_none());
    }

    #[test]
    fn compares_tokens() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abc", b"abd"));
        assert!(!same_token(b"abc", b"ab"));
    }

    #[test]
    fn origins() {
        let allowed = vec!["http://localhost:5173/".to_string()];
        assert!(origin_allowed(&allowed, "http://localhost:5173"));
        assert!(!origin_allowed(&allowed, "http://evil.test"));
        assert!(!origin_allowed(&[], "null"));
    }
}
